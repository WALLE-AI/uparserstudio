use crate::ooxml::attribute;
use crate::package::Package;
use crate::{
    Asset, AssetId, Block, CanonicalDocument, Cell, CellSlot, CellValueKind, DocumentError,
    DocumentFormat, DocumentUnit, ImageSource, Inline, LinkTarget, List, ListItem, ListMarker,
    Note, NoteKind, ParseOptions, ParseWarning, Table, TableKind, UnitKind, WarningCode,
};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

const CONTENT_PART: &str = "content.xml";

pub(crate) fn parse(
    bytes: &[u8],
    format: DocumentFormat,
    options: &ParseOptions,
) -> Result<CanonicalDocument, DocumentError> {
    let mut package = Package::open(bytes, &options.limits)?;
    if package
        .read("META-INF/manifest.xml")?
        .is_some_and(|xml| contains_encryption(&xml))
    {
        return Err(DocumentError::Encrypted);
    }

    let xml = package.read_required(CONTENT_PART)?;
    let styles_xml = package.read("styles.xml")?;
    let (assets, asset_ids) = load_assets(&mut package, options)?;
    let mut document = CanonicalDocument::new(format);
    document.metadata.variant = Some(
        match format {
            DocumentFormat::Odt => "odt",
            DocumentFormat::Odp => "odp",
            _ => unreachable!(),
        }
        .to_owned(),
    );
    document.assets = assets;
    if let Some(meta) = package.read("meta.xml")? {
        parse_metadata(&meta, options, &mut document)?;
    }
    let mut list_styles = parse_list_styles(&xml, options)?;
    if let Some(styles_xml) = styles_xml {
        list_styles.extend(parse_list_styles(&styles_xml, options)?);
    }
    parse_content(
        &xml,
        format,
        options,
        &asset_ids,
        &list_styles,
        &mut document,
    )?;
    Ok(document)
}

fn parse_metadata(
    xml: &[u8],
    options: &ParseOptions,
    document: &mut CanonicalDocument,
) -> Result<(), DocumentError> {
    let mut reader = Reader::from_reader(xml);
    reader.trim_text(true);
    let mut field: Option<Vec<u8>> = None;
    let mut nodes = 0usize;
    loop {
        nodes += 1;
        if nodes > options.limits.max_xml_nodes {
            return Err(limit("max_xml_nodes", nodes));
        }
        match reader.read_event() {
            Ok(Event::Start(event)) => match event.local_name().as_ref() {
                b"title" | b"creator" | b"subject" | b"language" | b"creation-date" | b"date"
                | b"generator" => {
                    field = Some(event.local_name().as_ref().to_vec());
                }
                _ => {}
            },
            Ok(Event::Text(text)) => {
                if let Some(field) = field.as_deref() {
                    let value = text
                        .unescape()
                        .map_err(|error| DocumentError::Malformed {
                            part: Some("meta.xml".to_owned()),
                            detail: error.to_string(),
                        })?
                        .into_owned();
                    match field {
                        b"title" => document.metadata.title = Some(value),
                        b"creator" => document.metadata.author = Some(value),
                        b"subject" => document.metadata.subject = Some(value),
                        b"language" => document.metadata.language = Some(value),
                        b"creation-date" => document.metadata.created_at = Some(value),
                        b"date" => document.metadata.modified_at = Some(value),
                        b"generator" => {
                            document
                                .metadata
                                .properties
                                .insert("generator".to_owned(), value);
                        }
                        _ => {}
                    }
                }
            }
            Ok(Event::End(_)) => field = None,
            Ok(Event::Eof) => break,
            Err(error) => {
                return Err(DocumentError::Malformed {
                    part: Some("meta.xml".to_owned()),
                    detail: error.to_string(),
                });
            }
            _ => {}
        }
    }
    Ok(())
}

fn parse_list_styles(
    xml: &[u8],
    options: &ParseOptions,
) -> Result<HashMap<String, ListMarker>, DocumentError> {
    let mut reader = Reader::from_reader(xml);
    let mut styles = HashMap::new();
    let mut current: Option<String> = None;
    let mut nodes = 0usize;
    loop {
        nodes += 1;
        if nodes > options.limits.max_xml_nodes {
            return Err(limit("max_xml_nodes", nodes));
        }
        match reader.read_event() {
            Ok(Event::Start(event)) if event.local_name().as_ref() == b"list-style" => {
                current = attribute(&event, b"name");
            }
            Ok(Event::Start(event) | Event::Empty(event)) => {
                let marker = match event.local_name().as_ref() {
                    b"list-level-style-number" => Some(ListMarker::Decimal),
                    b"list-level-style-bullet" | b"list-level-style-image" => {
                        Some(ListMarker::Bullet)
                    }
                    _ => None,
                };
                if let (Some(name), Some(marker)) = (current.as_ref(), marker) {
                    styles.entry(name.clone()).or_insert(marker);
                }
            }
            Ok(Event::End(event)) if event.local_name().as_ref() == b"list-style" => {
                current = None;
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(DocumentError::malformed(error.to_string())),
            _ => {}
        }
    }
    Ok(styles)
}

fn contains_encryption(xml: &[u8]) -> bool {
    let mut reader = Reader::from_reader(xml);
    loop {
        match reader.read_event() {
            Ok(Event::Start(event) | Event::Empty(event))
                if event.local_name().as_ref() == b"encryption-data" =>
            {
                return true;
            }
            Ok(Event::Eof) | Err(_) => return false,
            _ => {}
        }
    }
}

fn load_assets(
    package: &mut Package<'_>,
    options: &ParseOptions,
) -> Result<(Vec<Asset>, HashMap<String, AssetId>), DocumentError> {
    let names: Vec<String> = package
        .names()
        .filter(|name| image_media_type(name).is_some())
        .map(ToOwned::to_owned)
        .collect();
    let mut assets = Vec::new();
    let mut ids = HashMap::new();
    for name in names {
        let Some(bytes) = package.read(&name)? else {
            continue;
        };
        if bytes.len() > options.limits.max_asset_bytes {
            continue;
        }
        let sha256 = format!("{:x}", Sha256::digest(&bytes));
        let id = format!("asset-{}", &sha256[..16]);
        ids.insert(normalize_href(&name), id.clone());
        if !assets.iter().any(|asset: &Asset| asset.id == id) {
            assets.push(Asset {
                id,
                media_type: image_media_type(&name).unwrap().to_owned(),
                filename: name.rsplit('/').next().map(ToOwned::to_owned),
                byte_length: bytes.len(),
                sha256,
                bytes: options.include_assets.then_some(bytes),
            });
        }
    }
    Ok((assets, ids))
}

fn image_media_type(path: &str) -> Option<&'static str> {
    let path = path.to_ascii_lowercase();
    if path.ends_with(".png") {
        Some("image/png")
    } else if path.ends_with(".jpg") || path.ends_with(".jpeg") {
        Some("image/jpeg")
    } else if path.ends_with(".gif") {
        Some("image/gif")
    } else if path.ends_with(".svg") {
        Some("image/svg+xml")
    } else if path.ends_with(".webp") {
        Some("image/webp")
    } else if path.ends_with(".bmp") {
        Some("image/bmp")
    } else {
        None
    }
}

#[derive(Default)]
struct ParagraphBuilder {
    heading_level: Option<u8>,
    content: Vec<Inline>,
    link: Option<LinkBuilder>,
}

struct LinkBuilder {
    target: LinkTarget,
    content: Vec<Inline>,
}

struct ListBuilder {
    marker: ListMarker,
    items: Vec<ListItem>,
}

#[derive(Clone)]
struct RawCell {
    blocks: Vec<Block>,
    repeat: usize,
    row_span: usize,
    column_span: usize,
    covered: bool,
}

#[derive(Clone, Default)]
struct RawRow {
    cells: Vec<RawCell>,
    repeat: usize,
}

#[derive(Default)]
struct TableBuilder {
    rows: Vec<RawRow>,
    row: Option<RawRow>,
    cell: Option<RawCell>,
    name: Option<String>,
}

struct NoteBuilder {
    id: String,
    kind: NoteKind,
    blocks: Vec<Block>,
    outer_paragraph: Option<ParagraphBuilder>,
}

struct ParseState {
    units: Vec<DocumentUnit>,
    current_unit: Option<DocumentUnit>,
    paragraph: Option<ParagraphBuilder>,
    lists: Vec<ListBuilder>,
    table: Option<TableBuilder>,
    depth: usize,
    nodes: usize,
    expanded: u64,
    note: Option<NoteBuilder>,
    ignored_note_depth: usize,
}

fn parse_content(
    xml: &[u8],
    format: DocumentFormat,
    options: &ParseOptions,
    asset_ids: &HashMap<String, AssetId>,
    list_styles: &HashMap<String, ListMarker>,
    document: &mut CanonicalDocument,
) -> Result<(), DocumentError> {
    let mut state = ParseState {
        units: Vec::new(),
        current_unit: (format == DocumentFormat::Odt)
            .then(|| DocumentUnit::new(UnitKind::Flow, 0, None)),
        paragraph: None,
        lists: Vec::new(),
        table: None,
        depth: 0,
        nodes: 0,
        expanded: 0,
        note: None,
        ignored_note_depth: 0,
    };
    let mut reader = Reader::from_reader(xml);
    reader.trim_text(false);
    loop {
        let event = reader
            .read_event()
            .map_err(|error| DocumentError::Malformed {
                part: Some(CONTENT_PART.to_owned()),
                detail: error.to_string(),
            })?;
        state.nodes += 1;
        if state.nodes > options.limits.max_xml_nodes {
            return Err(limit("max_xml_nodes", state.nodes));
        }
        match event {
            Event::Start(event) => {
                state.depth += 1;
                if state.depth > options.limits.max_xml_depth {
                    return Err(limit("max_xml_depth", state.depth));
                }
                start_element(
                    &event,
                    format,
                    options,
                    asset_ids,
                    list_styles,
                    document,
                    &mut state,
                )?;
            }
            Event::Empty(event) => {
                start_element(
                    &event,
                    format,
                    options,
                    asset_ids,
                    list_styles,
                    document,
                    &mut state,
                )?;
                end_element(
                    event.local_name().as_ref(),
                    format,
                    options,
                    document,
                    &mut state,
                )?;
            }
            Event::Text(text) => {
                if let Some(paragraph) = state.paragraph.as_mut() {
                    let value = text.unescape().map_err(|error| DocumentError::Malformed {
                        part: Some(CONTENT_PART.to_owned()),
                        detail: error.to_string(),
                    })?;
                    push_inline(paragraph, Inline::text(value));
                }
            }
            Event::End(event) => {
                end_element(
                    event.local_name().as_ref(),
                    format,
                    options,
                    document,
                    &mut state,
                )?;
                state.depth = state.depth.saturating_sub(1);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if let Some(unit) = state.current_unit.take() {
        state.units.push(unit);
    }
    document.units = state.units;
    Ok(())
}

fn start_element(
    event: &BytesStart<'_>,
    format: DocumentFormat,
    options: &ParseOptions,
    asset_ids: &HashMap<String, AssetId>,
    list_styles: &HashMap<String, ListMarker>,
    document: &mut CanonicalDocument,
    state: &mut ParseState,
) -> Result<(), DocumentError> {
    if state.ignored_note_depth > 0 {
        state.ignored_note_depth += 1;
        return Ok(());
    }
    if event.local_name().as_ref() == b"note" && !options.include_notes {
        state.ignored_note_depth = 1;
        return Ok(());
    }
    match event.local_name().as_ref() {
        b"page" if format == DocumentFormat::Odp => {
            if let Some(unit) = state.current_unit.take() {
                state.units.push(unit);
            }
            state.current_unit = Some(DocumentUnit::new(
                UnitKind::Slide,
                state.units.len(),
                attribute(event, b"name"),
            ));
        }
        b"h" => {
            state.paragraph = Some(ParagraphBuilder {
                heading_level: Some(
                    attribute(event, b"outline-level")
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(1)
                        .clamp(1, 6),
                ),
                ..Default::default()
            });
        }
        b"p" if state.paragraph.is_none() => {
            state.paragraph = Some(ParagraphBuilder::default());
        }
        b"a" => {
            if let Some(paragraph) = state.paragraph.as_mut()
                && let Some(href) = attribute(event, b"href")
            {
                let target = if let Some(anchor) = href.strip_prefix('#') {
                    LinkTarget::Anchor(anchor.to_owned())
                } else {
                    LinkTarget::External(href)
                };
                paragraph.link = Some(LinkBuilder {
                    target,
                    content: Vec::new(),
                });
            }
        }
        b"list" => state.lists.push(ListBuilder {
            marker: list_marker(event, list_styles),
            items: Vec::new(),
        }),
        b"list-item" => {
            if let Some(list) = state.lists.last_mut() {
                list.items.push(ListItem { blocks: Vec::new() });
            }
        }
        b"table" if state.table.is_none() => {
            state.table = Some(TableBuilder {
                name: attribute(event, b"name"),
                ..Default::default()
            });
        }
        b"table-row" => {
            if let Some(table) = state.table.as_mut() {
                table.row = Some(RawRow {
                    repeat: repeat(event, b"number-rows-repeated", options, &mut state.expanded)?,
                    ..Default::default()
                });
            }
        }
        b"table-cell" | b"covered-table-cell" => {
            if let Some(table) = state.table.as_mut() {
                table.cell = Some(RawCell {
                    blocks: Vec::new(),
                    repeat: repeat(
                        event,
                        b"number-columns-repeated",
                        options,
                        &mut state.expanded,
                    )?,
                    row_span: span(event, b"number-rows-spanned"),
                    column_span: span(event, b"number-columns-spanned"),
                    covered: event.local_name().as_ref() == b"covered-table-cell",
                });
            }
        }
        b"image" => {
            let href = attribute(event, b"href").unwrap_or_default();
            let normalized = normalize_href(&href);
            let source = asset_ids
                .get(&normalized)
                .cloned()
                .map(ImageSource::Asset)
                .unwrap_or_else(|| ImageSource::External(href.clone()));
            if let Some(paragraph) = state.paragraph.as_mut() {
                push_inline(paragraph, Inline::Image { source, alt: None });
            } else {
                append_block(
                    Block::Figure {
                        asset_id: asset_ids.get(&normalized).cloned(),
                        alt: None,
                        caption: Vec::new(),
                    },
                    state,
                );
            }
        }
        b"line-break" => {
            if let Some(paragraph) = state.paragraph.as_mut() {
                push_inline(paragraph, Inline::LineBreak);
            }
        }
        b"s" => {
            if let Some(paragraph) = state.paragraph.as_mut() {
                let count = attribute(event, b"c")
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(1)
                    .min(options.limits.max_text_bytes);
                push_inline(paragraph, Inline::text(" ".repeat(count)));
            }
        }
        b"bookmark" | b"bookmark-start" => {
            if let (Some(paragraph), Some(id)) =
                (state.paragraph.as_mut(), attribute(event, b"name"))
            {
                push_inline(paragraph, Inline::Anchor { id });
            }
        }
        b"note" => {
            let id = attribute(event, b"id")
                .unwrap_or_else(|| format!("odf-note-{}", document.notes.len() + 1));
            if let Some(paragraph) = state.paragraph.as_mut() {
                push_inline(paragraph, Inline::NoteRef { id: id.clone() });
            }
            state.note = Some(NoteBuilder {
                id,
                kind: if attribute(event, b"note-class").as_deref() == Some("endnote") {
                    NoteKind::Endnote
                } else {
                    NoteKind::Footnote
                },
                blocks: Vec::new(),
                outer_paragraph: state.paragraph.take(),
            });
        }
        _ => {}
    }
    Ok(())
}

fn end_element(
    name: &[u8],
    format: DocumentFormat,
    options: &ParseOptions,
    document: &mut CanonicalDocument,
    state: &mut ParseState,
) -> Result<(), DocumentError> {
    if state.ignored_note_depth > 0 {
        state.ignored_note_depth -= 1;
        return Ok(());
    }
    match name {
        b"a" => {
            if let Some(paragraph) = state.paragraph.as_mut()
                && let Some(link) = paragraph.link.take()
            {
                paragraph.content.push(Inline::Link {
                    target: link.target,
                    content: link.content,
                });
            }
        }
        b"p" | b"h" => {
            if let Some(paragraph) = state.paragraph.take()
                && !paragraph.content.is_empty()
            {
                let block = match paragraph.heading_level {
                    Some(level) => Block::Heading {
                        level,
                        content: paragraph.content,
                    },
                    None => Block::Paragraph {
                        content: paragraph.content,
                    },
                };
                append_block(block, state);
            }
        }
        b"list" => {
            if let Some(list) = state.lists.pop() {
                append_block(
                    Block::List {
                        list: List {
                            marker: list.marker,
                            start: None,
                            items: list.items,
                        },
                    },
                    state,
                );
            }
        }
        b"table-cell" | b"covered-table-cell" => {
            if let Some(table) = state.table.as_mut()
                && let (Some(row), Some(cell)) = (table.row.as_mut(), table.cell.take())
            {
                row.cells.push(cell);
            }
        }
        b"table-row" => {
            if let Some(table) = state.table.as_mut()
                && let Some(row) = table.row.take()
            {
                table.rows.push(row);
            }
        }
        b"table" => {
            if let Some(table) = state.table.take() {
                let caption = table.name.clone().map(|name| vec![Inline::text(name)]);
                let table =
                    build_table(table, options, &mut state.expanded, &mut document.warnings)?;
                append_block(
                    Block::Table {
                        table: Table { caption, ..table },
                    },
                    state,
                );
            }
        }
        b"page" if format == DocumentFormat::Odp => {
            if let Some(unit) = state.current_unit.take() {
                state.units.push(unit);
            }
        }
        b"note" => {
            if let Some(note) = state.note.take() {
                document.notes.push(Note {
                    id: note.id,
                    kind: note.kind,
                    blocks: note.blocks,
                });
                state.paragraph = note.outer_paragraph;
            }
        }
        _ => {}
    }
    Ok(())
}

fn append_block(block: Block, state: &mut ParseState) {
    if let Some(note) = state.note.as_mut() {
        note.blocks.push(block);
    } else if let Some(list) = state.lists.last_mut() {
        if list.items.is_empty() {
            list.items.push(ListItem { blocks: Vec::new() });
        }
        list.items.last_mut().unwrap().blocks.push(block);
    } else if let Some(cell) = state.table.as_mut().and_then(|table| table.cell.as_mut()) {
        cell.blocks.push(block);
    } else if let Some(unit) = state.current_unit.as_mut() {
        unit.blocks.push(block);
    }
}

fn push_inline(paragraph: &mut ParagraphBuilder, inline: Inline) {
    if let Some(link) = paragraph.link.as_mut() {
        link.content.push(inline);
    } else {
        paragraph.content.push(inline);
    }
}

fn list_marker(event: &BytesStart<'_>, styles: &HashMap<String, ListMarker>) -> ListMarker {
    if let Some(marker) =
        attribute(event, b"style-name").and_then(|name| styles.get(&name).copied())
    {
        return marker;
    }
    match attribute(event, b"style-name").as_deref() {
        Some(name) if name.to_ascii_lowercase().contains("number") => ListMarker::Decimal,
        _ => ListMarker::Bullet,
    }
}

fn repeat(
    event: &BytesStart<'_>,
    name: &[u8],
    options: &ParseOptions,
    expanded: &mut u64,
) -> Result<usize, DocumentError> {
    let value = attribute(event, name)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(1);
    *expanded = expanded.saturating_add(value);
    if *expanded > options.limits.max_expansion {
        return Err(DocumentError::ResourceLimit {
            limit: "max_expansion",
            detail: format!("ODF repeated structures expand to at least {expanded} items"),
        });
    }
    usize::try_from(value).map_err(|_| limit("max_expansion", value))
}

fn span(event: &BytesStart<'_>, name: &[u8]) -> usize {
    attribute(event, name)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1)
        .max(1)
}

fn build_table(
    raw: TableBuilder,
    options: &ParseOptions,
    expanded: &mut u64,
    warnings: &mut Vec<ParseWarning>,
) -> Result<Table, DocumentError> {
    let mut rows = Vec::new();
    for row in raw.rows {
        for _ in 0..row.repeat.max(1) {
            let mut cells = Vec::new();
            for cell in &row.cells {
                for _ in 0..cell.repeat.max(1) {
                    cells.push(cell.clone());
                }
            }
            *expanded = expanded.saturating_add(cells.len() as u64);
            if *expanded > options.limits.max_expansion {
                return Err(DocumentError::ResourceLimit {
                    limit: "max_expansion",
                    detail: "ODF table expansion exceeds configured budget".to_owned(),
                });
            }
            rows.push(cells);
        }
    }
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut grid: Vec<Vec<Option<CellSlot>>> = vec![vec![None; columns]; rows.len()];
    for (row_index, row) in rows.into_iter().enumerate() {
        let mut column = 0usize;
        for cell in row {
            while column < columns && grid[row_index][column].is_some() {
                column += 1;
            }
            if column >= columns {
                break;
            }
            if cell.covered {
                column += 1;
                continue;
            }
            let row_span = cell.row_span.min(grid.len() - row_index).max(1);
            let column_span = cell.column_span.min(columns - column).max(1);
            if row_span != cell.row_span || column_span != cell.column_span {
                warnings.push(ParseWarning {
                    code: WarningCode::InvalidSpanClamped,
                    part: Some(CONTENT_PART.to_owned()),
                    message: "ODF table span exceeded the table grid and was clamped".to_owned(),
                });
            }
            grid[row_index][column] = Some(CellSlot::Origin(Cell {
                row_span,
                column_span,
                value_kind: if cell.blocks.is_empty() {
                    CellValueKind::Empty
                } else {
                    CellValueKind::Text
                },
                formula: None,
                blocks: cell.blocks,
            }));
            for (covered_row, grid_row) in
                grid.iter_mut().enumerate().skip(row_index).take(row_span)
            {
                for (covered_column, slot) in grid_row
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
            column += column_span;
        }
    }
    let grid: Vec<Vec<CellSlot>> = grid
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|slot| {
                    slot.unwrap_or_else(|| CellSlot::Origin(Cell::text("", CellValueKind::Empty)))
                })
                .collect()
        })
        .collect();
    Ok(Table {
        kind: TableKind::Data,
        rows: grid.len(),
        columns,
        header_rows: 0,
        grid,
        caption: None,
    })
}

fn normalize_href(value: &str) -> String {
    value
        .trim_start_matches("./")
        .replace("%20", " ")
        .replace('\\', "/")
}

fn limit(limit_name: &'static str, value: impl std::fmt::Display) -> DocumentError {
    DocumentError::ResourceLimit {
        limit: limit_name,
        detail: format!("ODF content reached {value}"),
    }
}
