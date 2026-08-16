use crate::ooxml::{
    Relationships, attribute, load_image_relationships, load_relationships,
    load_root_relationships, main_part, relationship_id, resolve_internal_target,
};
use crate::package::Package;
use crate::{
    AssetId, Block, CanonicalDocument, Cell, CellSlot, CellValueKind, DocumentError,
    DocumentFormat, DocumentUnit, Inline, List, ListItem, ListMarker, Note, NoteKind, ParseOptions,
    ParseWarning, Table, TableKind, UnitKind, WarningCode,
};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use std::collections::HashMap;

/// Conventional presentation-part path, used only when the package declares
/// no root `officeDocument` relationship.
const PRESENTATION_PART: &str = "ppt/presentation.xml";

pub(crate) fn parse(
    bytes: &[u8],
    options: &ParseOptions,
) -> Result<CanonicalDocument, DocumentError> {
    let mut package = Package::open(bytes, &options.limits)?;
    let mut document = CanonicalDocument::new(DocumentFormat::Pptx);
    document.metadata.variant = Some("pptx".to_owned());
    let slide_names = ordered_slide_parts(&mut package, options, &mut document.warnings)?;

    for (index, name) in slide_names.into_iter().enumerate() {
        let xml = package.read_required(&name)?;
        let relationships = load_relationships(&mut package, &name, options)?;
        let image_ids = load_image_relationships(
            &mut package,
            &name,
            &relationships,
            options,
            &mut document.assets,
            &mut document.warnings,
        )?;
        let blocks = parse_slide_xml(&xml, &name, options, &image_ids, &mut document.warnings)?;
        let label = blocks.iter().find_map(|block| match block {
            Block::Heading { content, .. } => inline_text(content),
            _ => None,
        });
        document.units.push(DocumentUnit {
            kind: UnitKind::Slide,
            index,
            label: label.or_else(|| Some(format!("Slide {}", index + 1))),
            blocks,
        });

        let fallback_note = format!("ppt/notesSlides/notesSlide{}.xml", index + 1);
        let note_name = relationship_target(&name, &relationships, "/notesSlide").or_else(|| {
            package
                .names()
                .any(|candidate| candidate == fallback_note)
                .then_some(fallback_note)
        });
        if options.include_notes
            && let Some(note_name) = note_name
            && let Some(note_xml) = package.read(&note_name)?
        {
            let note_blocks = parse_slide_xml(
                &note_xml,
                &note_name,
                options,
                &HashMap::new(),
                &mut document.warnings,
            )?
            .into_iter()
            .filter(|block| !is_placeholder_note(block))
            .collect::<Vec<_>>();
            if !note_blocks.is_empty() {
                document.notes.push(Note {
                    id: format!("slide-{}-notes", index + 1),
                    kind: NoteKind::SpeakerNote,
                    blocks: note_blocks,
                });
            }
        }
    }
    if document.units.is_empty() {
        return Err(DocumentError::MissingPart {
            part: "ppt/slides/slideN.xml".to_owned(),
        });
    }
    Ok(document)
}

fn ordered_slide_parts(
    package: &mut Package<'_>,
    options: &ParseOptions,
    warnings: &mut Vec<ParseWarning>,
) -> Result<Vec<String>, DocumentError> {
    // The presentation part is named by the root `officeDocument`
    // relationship; `ppt/presentation.xml` is only the convention.
    let root_relationships = load_root_relationships(package, options)?;
    let presentation_part =
        main_part(&root_relationships).unwrap_or_else(|| PRESENTATION_PART.to_owned());

    if let Some(xml) = package.read(&presentation_part)? {
        let relationships = load_relationships(package, &presentation_part, options)?;
        let ids = parse_slide_relationship_ids(&xml, &presentation_part, options)?;
        let mut parts = Vec::new();
        for id in ids {
            let Some(relationship) = relationships.get(&id) else {
                warnings.push(ParseWarning {
                    code: WarningCode::BrokenRelationship,
                    part: Some(presentation_part.clone()),
                    message: format!("slide relationship {id} is missing"),
                });
                continue;
            };
            if let Some(target) = resolve_internal_target(&presentation_part, &relationship.target)
            {
                parts.push(target);
            }
        }
        if !parts.is_empty() {
            return Ok(parts);
        }
        warnings.push(ParseWarning {
            code: WarningCode::BrokenRelationship,
            part: Some(presentation_part),
            message: "no slide could be resolved through presentation relationships; \
                      falling back to slide-part filename order"
                .to_owned(),
        });
    }

    let mut names = package
        .names()
        .filter(|name| is_numbered_part(name, "ppt/slides/slide", ".xml"))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    names.sort_by_key(|name| part_number(name, "ppt/slides/slide", ".xml"));
    Ok(names)
}

fn parse_slide_relationship_ids(
    xml: &[u8],
    part: &str,
    options: &ParseOptions,
) -> Result<Vec<String>, DocumentError> {
    let mut reader = Reader::from_reader(xml);
    let mut ids = Vec::new();
    let mut nodes = 0usize;
    loop {
        nodes += 1;
        enforce_node_limit(nodes, part, options)?;
        match reader.read_event() {
            Ok(Event::Start(event) | Event::Empty(event))
                if event.local_name().as_ref() == b"sldId" =>
            {
                // `<p:sldId id="256" r:id="rId4"/>` carries two `id`-named
                // attributes; only the namespace-prefixed one is the
                // relationship reference.
                if let Some(id) = relationship_id(&event) {
                    ids.push(id);
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(malformed_part(part, error)),
            _ => {}
        }
    }
    Ok(ids)
}

fn relationship_target(
    source_part: &str,
    relationships: &Relationships,
    kind_suffix: &str,
) -> Option<String> {
    relationships
        .values()
        .find(|relationship| !relationship.external && relationship.kind.ends_with(kind_suffix))
        .and_then(|relationship| resolve_internal_target(source_part, &relationship.target))
}

#[derive(Default)]
struct ShapeState {
    title: bool,
    paragraphs: Vec<Paragraph>,
}

#[derive(Default)]
struct PictureState {
    asset_id: Option<AssetId>,
    alt: Option<String>,
}

#[derive(Default)]
struct Paragraph {
    text: String,
    listed: bool,
    level: u8,
}

#[derive(Default)]
struct RawCell {
    blocks: Vec<Block>,
    column_span: usize,
    row_span: usize,
    horizontal_merge: bool,
    vertical_merge: bool,
}

#[derive(Default)]
struct RawTable {
    rows: Vec<Vec<RawCell>>,
    row: Vec<RawCell>,
    cell: Option<RawCell>,
}

fn parse_slide_xml(
    xml: &[u8],
    part: &str,
    options: &ParseOptions,
    image_ids: &HashMap<String, AssetId>,
    warnings: &mut Vec<ParseWarning>,
) -> Result<Vec<Block>, DocumentError> {
    let mut reader = Reader::from_reader(xml);
    reader.trim_text(false);
    let mut output = Vec::new();
    let mut shape: Option<ShapeState> = None;
    let mut picture: Option<PictureState> = None;
    let mut table: Option<RawTable> = None;
    let mut paragraph: Option<Paragraph> = None;
    let mut in_text = false;
    let mut nodes = 0usize;
    let mut depth = 0usize;
    loop {
        nodes += 1;
        enforce_node_limit(nodes, part, options)?;
        match reader.read_event() {
            Ok(Event::Start(event)) => {
                depth += 1;
                enforce_depth_limit(depth, part, options)?;
                match event.local_name().as_ref() {
                    b"sp" => shape = Some(ShapeState::default()),
                    b"pic" => picture = Some(PictureState::default()),
                    b"tbl" => table = Some(RawTable::default()),
                    b"tr" if table.is_some() => table.as_mut().unwrap().row = Vec::new(),
                    b"tc" if table.is_some() => {
                        table.as_mut().unwrap().cell = Some(raw_cell(&event));
                    }
                    b"p" => paragraph = Some(Paragraph::default()),
                    b"t" => in_text = true,
                    b"pPr" => update_paragraph_properties(&event, &mut paragraph),
                    b"ph" => mark_title(&event, &mut shape),
                    b"cNvPr" => update_picture_alt(&event, &mut picture),
                    b"blip" => {
                        update_picture_asset(&event, image_ids, &mut picture, warnings, part)
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(event)) => match event.local_name().as_ref() {
                b"ph" => mark_title(&event, &mut shape),
                b"pPr" => update_paragraph_properties(&event, &mut paragraph),
                b"buChar" | b"buAutoNum" => {
                    if let Some(paragraph) = paragraph.as_mut() {
                        paragraph.listed = true;
                    }
                }
                b"br" => {
                    if let Some(paragraph) = paragraph.as_mut() {
                        paragraph.text.push('\n');
                    }
                }
                b"cNvPr" => update_picture_alt(&event, &mut picture),
                b"blip" => update_picture_asset(&event, image_ids, &mut picture, warnings, part),
                _ => {}
            },
            Ok(Event::Text(text)) if in_text => {
                let value = text
                    .unescape()
                    .map_err(|error| malformed_part(part, error))?;
                if let Some(paragraph) = paragraph.as_mut() {
                    paragraph.text.push_str(&value);
                }
            }
            Ok(Event::End(event)) => {
                depth = depth.saturating_sub(1);
                match event.local_name().as_ref() {
                    b"t" => in_text = false,
                    b"p" => {
                        if let Some(paragraph) = paragraph.take()
                            && !paragraph.text.trim().is_empty()
                        {
                            if let Some(cell) = table.as_mut().and_then(|table| table.cell.as_mut())
                            {
                                cell.blocks.push(Block::paragraph(paragraph.text));
                            } else if let Some(shape) = shape.as_mut() {
                                shape.paragraphs.push(paragraph);
                            }
                        }
                    }
                    b"tc" => {
                        if let Some(table) = table.as_mut()
                            && let Some(cell) = table.cell.take()
                        {
                            table.row.push(cell);
                        }
                    }
                    b"tr" => {
                        if let Some(table) = table.as_mut() {
                            table.rows.push(std::mem::take(&mut table.row));
                        }
                    }
                    b"tbl" => {
                        if let Some(table) = table.take() {
                            output.push(build_table(table, part, warnings));
                        }
                    }
                    b"sp" => {
                        if let Some(shape) = shape.take() {
                            append_shape(&mut output, shape);
                        }
                    }
                    b"pic" => {
                        if let Some(picture) = picture.take()
                            && picture.asset_id.is_some()
                        {
                            output.push(Block::Figure {
                                asset_id: picture.asset_id,
                                alt: picture.alt,
                                caption: Vec::new(),
                            });
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => {
                // Same recovery policy as DOCX: a slide that already produced
                // content keeps it; a slide that produced nothing is a real
                // parse failure.
                if output.is_empty() {
                    return Err(malformed_part(part, error));
                }
                warnings.push(ParseWarning {
                    code: WarningCode::TruncatedContent,
                    part: Some(part.to_owned()),
                    message: format!("slide truncated at a malformed node: {error}"),
                });
                break;
            }
            _ => {}
        }
    }
    Ok(output)
}

fn enforce_depth_limit(
    depth: usize,
    part: &str,
    options: &ParseOptions,
) -> Result<(), DocumentError> {
    if depth > options.limits.max_xml_depth {
        Err(DocumentError::ResourceLimit {
            limit: "max_xml_depth",
            detail: format!("{part} nests elements {depth} deep"),
        })
    } else {
        Ok(())
    }
}

fn raw_cell(event: &BytesStart<'_>) -> RawCell {
    RawCell {
        column_span: numeric_attribute(event, b"gridSpan").unwrap_or(1).max(1),
        row_span: numeric_attribute(event, b"rowSpan").unwrap_or(1).max(1),
        horizontal_merge: boolean_attribute(event, b"hMerge"),
        vertical_merge: boolean_attribute(event, b"vMerge"),
        ..Default::default()
    }
}

fn build_table(table: RawTable, part: &str, warnings: &mut Vec<ParseWarning>) -> Block {
    let rows = table.rows.len();
    let columns = table.rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut grid: Vec<Vec<Option<CellSlot>>> = vec![vec![None; columns]; rows];
    for (row_index, row) in table.rows.into_iter().enumerate() {
        for (column, raw) in row.into_iter().enumerate() {
            if grid[row_index][column].is_some() {
                continue;
            }
            if raw.horizontal_merge || raw.vertical_merge {
                let origin = if raw.horizontal_merge && column > 0 {
                    origin_at(&grid, row_index, column - 1)
                } else if raw.vertical_merge && row_index > 0 {
                    origin_at(&grid, row_index - 1, column)
                } else {
                    None
                };
                if let Some((origin_row, origin_column)) = origin {
                    grid[row_index][column] = Some(CellSlot::Covered {
                        origin_row,
                        origin_column,
                    });
                    if raw.vertical_merge
                        && let Some(CellSlot::Origin(cell)) =
                            grid[origin_row][origin_column].as_mut()
                    {
                        cell.row_span = cell.row_span.max(row_index - origin_row + 1);
                    }
                    continue;
                }
                warnings.push(ParseWarning {
                    code: WarningCode::InvalidSpanClamped,
                    part: Some(part.to_owned()),
                    message: format!(
                        "table merge at row {row_index}, column {column} has no origin"
                    ),
                });
            }
            let row_span = raw.row_span.min(rows.saturating_sub(row_index)).max(1);
            let column_span = raw.column_span.min(columns.saturating_sub(column)).max(1);
            // A span wider than the grid is bounded here rather than allowed
            // to drive allocation, but it is still a source defect worth
            // surfacing — silently clamping hides a malformed or hostile deck.
            if row_span != raw.row_span || column_span != raw.column_span {
                warnings.push(ParseWarning {
                    code: WarningCode::InvalidSpanClamped,
                    part: Some(part.to_owned()),
                    message: format!(
                        "table cell at row {row_index}, column {column} declares a \
                         {}x{} span that exceeds the {rows}x{columns} grid and was clamped",
                        raw.row_span, raw.column_span
                    ),
                });
            }
            grid[row_index][column] = Some(CellSlot::Origin(Cell {
                row_span,
                column_span,
                value_kind: if raw.blocks.is_empty() {
                    CellValueKind::Empty
                } else {
                    CellValueKind::Text
                },
                formula: None,
                blocks: raw.blocks,
            }));
            for (covered_row, row_slots) in
                grid.iter_mut().enumerate().skip(row_index).take(row_span)
            {
                for (covered_column, slot) in row_slots
                    .iter_mut()
                    .enumerate()
                    .skip(column)
                    .take(column_span)
                {
                    if covered_row != row_index || covered_column != column {
                        *slot = Some(CellSlot::Covered {
                            origin_row: row_index,
                            origin_column: column,
                        });
                    }
                }
            }
        }
    }
    let grid = grid
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|slot| {
                    slot.unwrap_or_else(|| CellSlot::Origin(Cell::text("", CellValueKind::Empty)))
                })
                .collect()
        })
        .collect();
    Block::Table {
        table: Table {
            kind: TableKind::Data,
            rows,
            columns,
            header_rows: 0,
            grid,
            caption: None,
        },
    }
}

fn origin_at(grid: &[Vec<Option<CellSlot>>], row: usize, column: usize) -> Option<(usize, usize)> {
    match grid.get(row)?.get(column)?.as_ref()? {
        CellSlot::Origin(_) => Some((row, column)),
        CellSlot::Covered {
            origin_row,
            origin_column,
        } => Some((*origin_row, *origin_column)),
    }
}

fn append_shape(output: &mut Vec<Block>, shape: ShapeState) {
    for (index, paragraph) in shape.paragraphs.into_iter().enumerate() {
        if shape.title && index == 0 {
            output.push(Block::Heading {
                level: 1,
                content: vec![Inline::text(paragraph.text)],
            });
        } else if paragraph.listed {
            append_list_item(output, paragraph.text, paragraph.level);
        } else {
            output.push(Block::paragraph(paragraph.text));
        }
    }
}

fn append_list_item(output: &mut Vec<Block>, text: String, _level: u8) {
    if let Some(Block::List { list }) = output.last_mut() {
        list.items.push(ListItem {
            blocks: vec![Block::paragraph(text)],
        });
    } else {
        output.push(Block::List {
            list: List {
                marker: ListMarker::Bullet,
                start: None,
                items: vec![ListItem {
                    blocks: vec![Block::paragraph(text)],
                }],
            },
        });
    }
}

fn update_paragraph_properties(event: &BytesStart<'_>, paragraph: &mut Option<Paragraph>) {
    if let Some(paragraph) = paragraph.as_mut()
        && let Some(level) = attribute(event, b"lvl").and_then(|value| value.parse().ok())
    {
        paragraph.listed = true;
        paragraph.level = level;
    }
}

fn update_picture_alt(event: &BytesStart<'_>, picture: &mut Option<PictureState>) {
    if let Some(picture) = picture.as_mut() {
        picture.alt = attribute(event, b"descr").or_else(|| attribute(event, b"name"));
    }
}

fn update_picture_asset(
    event: &BytesStart<'_>,
    image_ids: &HashMap<String, AssetId>,
    picture: &mut Option<PictureState>,
    warnings: &mut Vec<ParseWarning>,
    part: &str,
) {
    let Some(id) = attribute(event, b"embed") else {
        return;
    };
    if let Some(asset_id) = image_ids.get(&id) {
        if let Some(picture) = picture.as_mut() {
            picture.asset_id = Some(asset_id.clone());
        }
    } else {
        warnings.push(ParseWarning {
            code: WarningCode::BrokenRelationship,
            part: Some(part.to_owned()),
            message: format!("embedded image relationship {id} is unavailable"),
        });
    }
}

fn mark_title(event: &BytesStart<'_>, shape: &mut Option<ShapeState>) {
    let value = attribute(event, b"type").unwrap_or_else(|| "body".to_owned());
    if matches!(value.as_str(), "title" | "ctrTitle" | "subTitle")
        && let Some(shape) = shape.as_mut()
    {
        shape.title = true;
    }
}

fn inline_text(content: &[Inline]) -> Option<String> {
    let text = content
        .iter()
        .filter_map(|inline| match inline {
            Inline::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    (!text.is_empty()).then_some(text)
}

fn is_placeholder_note(block: &Block) -> bool {
    matches!(block, Block::Paragraph { content } if inline_text(content).is_some_and(|text| {
        matches!(text.trim(), "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
    }))
}

fn numeric_attribute(event: &BytesStart<'_>, name: &[u8]) -> Option<usize> {
    attribute(event, name).and_then(|value| value.parse().ok())
}

fn boolean_attribute(event: &BytesStart<'_>, name: &[u8]) -> bool {
    attribute(event, name)
        .is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "on"))
}

fn is_numbered_part(name: &str, prefix: &str, suffix: &str) -> bool {
    part_number(name, prefix, suffix) != usize::MAX
}

fn part_number(name: &str, prefix: &str, suffix: &str) -> usize {
    name.strip_prefix(prefix)
        .and_then(|value| value.strip_suffix(suffix))
        .and_then(|value| value.parse().ok())
        .unwrap_or(usize::MAX)
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
