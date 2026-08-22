use crate::ooxml::{
    REL_ENDNOTES, REL_FOOTER, REL_FOOTNOTES, REL_HEADER, REL_NUMBERING, REL_STYLES,
    ROOT_RELATIONSHIPS_PART, Relationships, attribute, load_image_relationships,
    load_relationships, load_root_relationships, main_part, related_part, relationship_id,
    resolve_internal_target,
};
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
    let mut document = CanonicalDocument::new(DocumentFormat::Docx);
    document.metadata.variant = Some("docx".to_owned());

    // The main part is whatever the package's root `officeDocument`
    // relationship points at. `word/document.xml` is only a convention; real
    // producers are free to place it elsewhere, and packages that do were
    // previously rejected outright.
    let root_relationships = load_root_relationships(&mut package, options)?;
    let document_part = match main_part(&root_relationships) {
        Some(part) => part,
        None => {
            document.warnings.push(ParseWarning {
                code: WarningCode::BrokenRelationship,
                part: Some(ROOT_RELATIONSHIPS_PART.to_owned()),
                message: "package declares no officeDocument relationship; \
                          falling back to the conventional main-part path"
                    .to_owned(),
            });
            DOCUMENT_PART.to_owned()
        }
    };
    let xml = package.read_required(&document_part)?;
    let relationships = load_relationships(&mut package, &document_part, options)?;

    let styles_part = related_part(&document_part, &relationships, REL_STYLES)
        .unwrap_or_else(|| sibling_part(&document_part, "styles.xml"));
    let numbering_part = related_part(&document_part, &relationships, REL_NUMBERING)
        .unwrap_or_else(|| sibling_part(&document_part, "numbering.xml"));

    // styles/numbering are optional parts: a corrupt one degrades formatting
    // but must not fail the document (recovery policy rule 2).
    let styles = optional_part(
        &mut package,
        &styles_part,
        options,
        &mut document.warnings,
        parse_styles,
    )?;
    let numbering = optional_part(
        &mut package,
        &numbering_part,
        options,
        &mut document.warnings,
        parse_numbering,
    )?;

    let image_ids = load_image_relationships(
        &mut package,
        &document_part,
        &relationships,
        options,
        &mut document.assets,
        &mut document.warnings,
    )?;
    let related_blocks = load_related_blocks(
        &mut package,
        &document_part,
        &relationships,
        options,
        &mut document.warnings,
    )?;
    let context = DocxContext {
        styles: &styles,
        numbering: &numbering,
        relationships: &relationships,
        image_ids: &image_ids,
        related_blocks: &related_blocks,
    };
    let mut unit = DocumentUnit::new(UnitKind::Flow, 0, None);

    // Running headers/footers live in their own parts, reached through the
    // main part's relationships. They repeat on every page, so they stay out
    // of the body unless the caller asks: a header emitted once per page
    // would otherwise dominate the extracted text.
    let (headers, footers) = if options.include_headers_footers {
        collect_headers_and_footers(
            &mut package,
            &document_part,
            &relationships,
            options,
            &context,
            &mut document.warnings,
        )?
    } else {
        (Vec::new(), Vec::new())
    };

    unit.blocks.extend(headers);
    parse_document_xml(
        &xml,
        &document_part,
        &mut unit.blocks,
        &mut document.warnings,
        options,
        &context,
    )?;
    unit.blocks.extend(footers);
    document.units.push(unit);

    if options.include_notes {
        for (rel, fallback, kind, prefix) in [
            (
                REL_FOOTNOTES,
                "footnotes.xml",
                NoteKind::Footnote,
                "footnote",
            ),
            (REL_ENDNOTES, "endnotes.xml", NoteKind::Endnote, "endnote"),
        ] {
            let part = related_part(&document_part, &relationships, rel)
                .unwrap_or_else(|| sibling_part(&document_part, fallback));
            let Some(xml) = package.read(&part)? else {
                continue;
            };
            // Notes are optional content: a broken notes part loses the notes,
            // not the document.
            match parse_notes(&xml, &part, kind, prefix, options) {
                Ok(notes) => document.notes.extend(notes),
                Err(error @ DocumentError::ResourceLimit { .. }) => return Err(error),
                Err(error) => document.warnings.push(ParseWarning {
                    code: WarningCode::OptionalPartSkipped,
                    part: Some(part),
                    message: format!("notes part skipped: {error}"),
                }),
            }
        }
    }
    Ok(document)
}

/// Parse every header and footer part related to the main document.
///
/// Returns them separately so the caller can place headers before the body
/// and footers after it, which is the only ordering that reads sensibly once
/// pagination is gone.
fn collect_headers_and_footers(
    package: &mut Package<'_>,
    document_part: &str,
    relationships: &Relationships,
    options: &ParseOptions,
    context: &DocxContext<'_>,
    warnings: &mut Vec<ParseWarning>,
) -> Result<(Vec<Block>, Vec<Block>), DocumentError> {
    let mut headers = Vec::new();
    let mut footers = Vec::new();
    // Relationship ids are sorted so repeated runs place the parts in a
    // stable order; the package itself imposes none.
    let mut related: Vec<_> = relationships.iter().collect();
    related.sort_by_key(|(id, _)| *id);

    for (id, relationship) in related {
        if relationship.external {
            continue;
        }
        let target = if relationship.kind.ends_with(REL_HEADER) {
            &mut headers
        } else if relationship.kind.ends_with(REL_FOOTER) {
            &mut footers
        } else {
            continue;
        };
        let Some(part) = crate::ooxml::resolve_internal_target(document_part, &relationship.target)
        else {
            warnings.push(ParseWarning {
                code: WarningCode::BrokenRelationship,
                part: Some(document_part.to_owned()),
                message: format!("header/footer relationship {id} escapes the package"),
            });
            continue;
        };
        let Some(xml) = package.read(&part)? else {
            continue;
        };
        // A broken header is not worth failing the document over.
        let mut blocks = Vec::new();
        match parse_document_xml(&xml, &part, &mut blocks, warnings, options, context) {
            Ok(()) => target.extend(blocks),
            Err(error @ DocumentError::ResourceLimit { .. }) => return Err(error),
            Err(error) => warnings.push(ParseWarning {
                code: WarningCode::OptionalPartSkipped,
                part: Some(part),
                message: format!("header/footer part skipped: {error}"),
            }),
        }
    }
    Ok((headers, footers))
}

/// A part next to `reference` in the same package folder. Used only as a
/// fallback when the corresponding relationship is absent.
fn sibling_part(reference: &str, filename: &str) -> String {
    match reference.rsplit_once('/') {
        Some((directory, _)) => format!("{directory}/{filename}"),
        None => filename.to_owned(),
    }
}

/// Read and parse an optional part, downgrading a parse failure to a warning.
/// `ResourceLimit` is never downgraded — it is always fatal.
fn optional_part<T: Default>(
    package: &mut Package<'_>,
    part: &str,
    options: &ParseOptions,
    warnings: &mut Vec<ParseWarning>,
    parse: fn(&[u8], &ParseOptions) -> Result<T, DocumentError>,
) -> Result<T, DocumentError> {
    let Some(xml) = package.read(part)? else {
        return Ok(T::default());
    };
    match parse(&xml, options) {
        Ok(value) => Ok(value),
        Err(error @ DocumentError::ResourceLimit { .. }) => Err(error),
        Err(error) => {
            warnings.push(ParseWarning {
                code: WarningCode::OptionalPartSkipped,
                part: Some(part.to_owned()),
                message: format!("optional part skipped: {error}"),
            });
            Ok(T::default())
        }
    }
}

struct DocxContext<'a> {
    styles: &'a HashMap<String, StyleDef>,
    numbering: &'a HashMap<(String, u8), NumberDef>,
    relationships: &'a Relationships,
    image_ids: &'a HashMap<String, AssetId>,
    related_blocks: &'a HashMap<String, Vec<Block>>,
}

fn load_related_blocks(
    package: &mut Package<'_>,
    document_part: &str,
    relationships: &Relationships,
    options: &ParseOptions,
    warnings: &mut Vec<ParseWarning>,
) -> Result<HashMap<String, Vec<Block>>, DocumentError> {
    let mut blocks = HashMap::new();
    for (id, relationship) in relationships {
        if relationship.external {
            continue;
        }
        let parser: Option<fn(&[u8], &str, &ParseOptions) -> Result<Vec<Block>, DocumentError>> =
            if relationship.kind.ends_with("/chart") {
                Some(parse_chart_xml)
            } else if relationship.kind.ends_with("/diagramData") {
                Some(parse_diagram_xml)
            } else {
                None
            };
        let Some(parser) = parser else { continue };
        let Some(part) = resolve_internal_target(document_part, &relationship.target) else {
            warnings.push(ParseWarning {
                code: WarningCode::BrokenRelationship,
                part: Some(document_part.to_owned()),
                message: format!("rich-object relationship {id} escapes the package"),
            });
            continue;
        };
        let Some(xml) = package.read(&part)? else {
            warnings.push(ParseWarning {
                code: WarningCode::BrokenRelationship,
                part: Some(part),
                message: format!("rich-object relationship {id} target is missing"),
            });
            continue;
        };
        match parser(&xml, &part, options) {
            Ok(parsed) if !parsed.is_empty() => {
                blocks.insert(id.clone(), parsed);
            }
            Ok(_) => {}
            Err(error @ DocumentError::ResourceLimit { .. }) => return Err(error),
            Err(error) => warnings.push(ParseWarning {
                code: WarningCode::OptionalPartSkipped,
                part: Some(part),
                message: format!("rich-object part skipped: {error}"),
            }),
        }
    }
    Ok(blocks)
}

#[derive(Default)]
struct ChartSeries {
    name: String,
    categories: Vec<String>,
    values: Vec<String>,
}

fn parse_chart_xml(
    xml: &[u8],
    part: &str,
    options: &ParseOptions,
) -> Result<Vec<Block>, DocumentError> {
    let mut reader = Reader::from_reader(xml);
    reader.trim_text(true);
    let mut stack: Vec<Vec<u8>> = Vec::new();
    let mut titles = Vec::new();
    let mut series = Vec::new();
    let mut current_series: Option<ChartSeries> = None;
    let mut nodes = 0usize;

    loop {
        nodes += 1;
        enforce_node_limit(nodes, part, options)?;
        match reader.read_event() {
            Ok(Event::Start(event)) => {
                enforce_depth_limit(stack.len() + 1, part, options)?;
                let name = event.local_name().as_ref().to_vec();
                if name == b"ser" {
                    current_series = Some(ChartSeries::default());
                }
                stack.push(name);
            }
            Ok(Event::Text(text)) => {
                let value = text
                    .unescape()
                    .map_err(|error| malformed_part(part, error))?
                    .trim()
                    .to_owned();
                if value.is_empty() {
                    continue;
                }
                match stack.last().map(Vec::as_slice) {
                    Some(b"t") if current_series.is_none() => titles.push(value),
                    Some(b"v") => {
                        if let Some(series) = current_series.as_mut() {
                            if stack.iter().any(|name| name.as_slice() == b"tx") {
                                series.name = value;
                            } else if stack.iter().any(|name| name.as_slice() == b"cat") {
                                series.categories.push(value);
                            } else if stack.iter().any(|name| name.as_slice() == b"val") {
                                series.values.push(value);
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::End(event)) => {
                if event.local_name().as_ref() == b"ser"
                    && let Some(value) = current_series.take()
                {
                    series.push(value);
                }
                stack.pop();
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(malformed_part(part, error)),
            _ => {}
        }
    }

    let mut blocks = Vec::new();
    if let Some(title) = titles.first().filter(|value| !value.is_empty()) {
        blocks.push(Block::Paragraph {
            content: vec![Inline::Text {
                text: title.clone(),
                style: Style {
                    bold: true,
                    ..Style::default()
                },
            }],
        });
    }
    if !series.is_empty() {
        let row_count = series
            .iter()
            .map(|series| series.categories.len().max(series.values.len()))
            .max()
            .unwrap_or(0);
        let column_count = series.len() + 1;
        let mut grid = Vec::with_capacity(row_count + 1);
        let category_title = titles.get(1).map(String::as_str).unwrap_or("Category");
        let mut header = vec![CellSlot::Origin(Cell::text(
            category_title,
            CellValueKind::Text,
        ))];
        header.extend(series.iter().enumerate().map(|(index, series)| {
            CellSlot::Origin(Cell::text(
                if series.name.is_empty() {
                    format!("Series {}", index + 1)
                } else {
                    series.name.clone()
                },
                CellValueKind::Text,
            ))
        }));
        grid.push(header);
        for row in 0..row_count {
            let category = series
                .iter()
                .find_map(|series| series.categories.get(row))
                .cloned()
                .unwrap_or_else(|| (row + 1).to_string());
            let mut cells = vec![CellSlot::Origin(Cell::text(category, CellValueKind::Text))];
            for series in &series {
                let value = series.values.get(row).cloned().unwrap_or_default();
                let kind = if value.parse::<f64>().is_ok() {
                    CellValueKind::Number
                } else if value.is_empty() {
                    CellValueKind::Empty
                } else {
                    CellValueKind::Text
                };
                cells.push(CellSlot::Origin(Cell::text(value, kind)));
            }
            grid.push(cells);
        }
        blocks.push(Block::Table {
            table: Table {
                kind: TableKind::Data,
                rows: row_count + 1,
                columns: column_count,
                header_rows: 1,
                grid,
                caption: None,
            },
        });
    }
    Ok(blocks)
}

fn parse_diagram_xml(
    xml: &[u8],
    part: &str,
    options: &ParseOptions,
) -> Result<Vec<Block>, DocumentError> {
    let mut reader = Reader::from_reader(xml);
    reader.trim_text(true);
    let mut in_text = false;
    let mut values = Vec::new();
    let mut nodes = 0usize;
    let mut depth = 0usize;
    loop {
        nodes += 1;
        enforce_node_limit(nodes, part, options)?;
        match reader.read_event() {
            Ok(Event::Start(event)) => {
                depth += 1;
                enforce_depth_limit(depth, part, options)?;
                in_text = event.local_name().as_ref() == b"t";
            }
            Ok(Event::Text(text)) if in_text => {
                let value = text
                    .unescape()
                    .map_err(|error| malformed_part(part, error))?
                    .trim()
                    .to_owned();
                if !value.is_empty() {
                    values.push(value);
                }
            }
            Ok(Event::End(event)) => {
                if event.local_name().as_ref() == b"t" {
                    in_text = false;
                }
                depth = depth.saturating_sub(1);
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(malformed_part(part, error)),
            _ => {}
        }
    }
    if values.is_empty() {
        return Ok(Vec::new());
    }
    Ok(vec![Block::List {
        list: List {
            marker: ListMarker::Bullet,
            start: None,
            items: values
                .into_iter()
                .map(|value| ListItem {
                    blocks: vec![Block::paragraph(value)],
                })
                .collect(),
        },
    }])
}

#[derive(Debug, Clone, Default)]
struct StyleDef {
    name: Option<String>,
    based_on: Option<String>,
    outline_level: Option<u8>,
    /// Run properties the style itself carries. Word expresses most emphasis
    /// through styles rather than direct `<w:b/>` formatting, so a parser that
    /// only reads direct properties loses it entirely.
    run_style: Style,
    is_paragraph_style: bool,
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
    part: &str,
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
    let mut pending_alt: Option<String> = None;
    let mut pending_related_blocks = Vec::new();
    let mut nodes = 0usize;
    let mut depth = 0usize;
    let mut counters = ListCounters::default();
    loop {
        nodes += 1;
        enforce_node_limit(nodes, part, options)?;
        match reader.read_event() {
            Ok(Event::Start(event)) => {
                depth += 1;
                enforce_depth_limit(depth, part, options)?;
                match event.local_name().as_ref() {
                    b"p" => {
                        paragraph = Some(Paragraph::default());
                        pending_alt = None;
                        pending_related_blocks.clear();
                    }
                    // A run starts from its paragraph style's run properties;
                    // `<w:rStyle>` and direct properties then layer on top.
                    b"r" => {
                        run_style = paragraph
                            .as_ref()
                            .and_then(|paragraph| paragraph.style.as_deref())
                            .map(|id| resolved_run_style(id, context.styles))
                            .unwrap_or_default();
                    }
                    b"rStyle" => {
                        if let Some(id) = attribute(&event, b"val") {
                            merge_run_style(
                                &mut run_style,
                                &resolved_run_style(&id, context.styles),
                            );
                        }
                    }
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
                                part: Some(part.to_owned()),
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
                    b"docPr" => set_drawing_alt(&event, &mut pending_alt),
                    b"chart" | b"relIds" => {
                        append_related_object(&event, context, &mut pending_related_blocks)
                    }
                    b"OLEObject" => append_ole_object(&event, &mut paragraph, &mut hyperlink),
                    _ => update_run_style(&event, &mut run_style),
                }
            }
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
                b"rStyle" => {
                    if let Some(id) = attribute(&event, b"val") {
                        merge_run_style(&mut run_style, &resolved_run_style(&id, context.styles));
                    }
                }
                b"pStyle" => set_paragraph_style(&event, &mut paragraph),
                b"numPr" => set_numbering_present(&mut paragraph),
                b"numId" => set_number_id(&event, &mut paragraph),
                b"ilvl" => set_number_level(&event, &mut paragraph),
                b"gridSpan" => set_grid_span(&event, &mut table),
                b"vMerge" => set_vertical_merge(&event, &mut table),
                b"tblHeader" => set_header(&mut table),
                b"docPr" => set_drawing_alt(&event, &mut pending_alt),
                b"chart" | b"relIds" => {
                    append_related_object(&event, context, &mut pending_related_blocks)
                }
                b"OLEObject" => append_ole_object(&event, &mut paragraph, &mut hyperlink),
                b"footnoteReference" => {
                    append_note_reference(&event, "footnote", &mut paragraph, &mut hyperlink)
                }
                b"endnoteReference" => {
                    append_note_reference(&event, "endnote", &mut paragraph, &mut hyperlink)
                }
                b"blip" => append_image(
                    &event,
                    pending_alt.take(),
                    context,
                    warnings,
                    &mut paragraph,
                    &mut hyperlink,
                ),
                _ => update_run_style(&event, &mut run_style),
            },
            Ok(Event::Text(text)) if in_text => {
                let value = text.unescape().map_err(|error| DocumentError::Malformed {
                    part: Some(part.to_owned()),
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
            Ok(Event::End(event)) => {
                depth = depth.saturating_sub(1);
                match event.local_name().as_ref() {
                    b"t" | b"instrText" => in_text = false,
                    b"hyperlink" => finish_hyperlink(&mut paragraph, &mut hyperlink),
                    b"p" => {
                        let container = match table.as_mut().and_then(|table| table.cell.as_mut()) {
                            Some(cell) => &mut cell.blocks,
                            None => &mut *output,
                        };
                        if let Some(paragraph) = paragraph.take()
                            && let Some(produced) =
                                paragraph_block(paragraph, context, &mut counters)
                        {
                            place_paragraph_output(produced, container);
                        }
                        container.append(&mut pending_related_blocks);
                        pending_alt = None;
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
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => {
                // Producers emit subtly non-well-formed bodies often enough
                // that discarding everything already recovered is the wrong
                // trade. Flush whatever is in flight, keep the blocks parsed
                // so far, and record why parsing stopped; only a body that
                // yielded nothing at all is fatal.
                if let Some(pending) = paragraph.take()
                    && let Some(produced) = paragraph_block(pending, context, &mut counters)
                {
                    place_paragraph_output(produced, output);
                }
                output.append(&mut pending_related_blocks);
                if let Some(pending) = table.take() {
                    output.push(build_table(pending, warnings));
                }
                if output.is_empty() {
                    return Err(DocumentError::Malformed {
                        part: Some(part.to_owned()),
                        detail: error.to_string(),
                    });
                }
                warnings.push(ParseWarning {
                    code: WarningCode::TruncatedContent,
                    part: Some(part.to_owned()),
                    message: format!("document body truncated at a malformed node: {error}"),
                });
                break;
            }
            _ => {}
        }
    }
    if table.is_some() {
        warnings.push(ParseWarning {
            code: WarningCode::TruncatedContent,
            part: Some(part.to_owned()),
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
    alt: Option<String>,
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
                alt,
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

fn set_drawing_alt(event: &BytesStart<'_>, alt: &mut Option<String>) {
    *alt = attribute(event, b"descr")
        .or_else(|| attribute(event, b"title"))
        .filter(|value| !value.trim().is_empty());
}

fn append_related_object(
    event: &BytesStart<'_>,
    context: &DocxContext<'_>,
    pending: &mut Vec<Block>,
) {
    let id = relationship_id(event).filter(|id| context.related_blocks.contains_key(id));
    let id = id.or_else(|| {
        event.attributes().flatten().find_map(|attribute| {
            let value = String::from_utf8_lossy(attribute.value.as_ref()).into_owned();
            context.related_blocks.contains_key(&value).then_some(value)
        })
    });
    if let Some(blocks) = id.and_then(|id| context.related_blocks.get(&id)) {
        pending.extend(blocks.iter().cloned());
    }
}

fn append_ole_object(
    event: &BytesStart<'_>,
    paragraph: &mut Option<Paragraph>,
    hyperlink: &mut Option<HyperlinkBuilder>,
) {
    let Some(program) = attribute(event, b"ProgID").filter(|value| !value.trim().is_empty()) else {
        return;
    };
    append_inline(
        paragraph,
        hyperlink,
        Inline::text(format!("Embedded object: {program}")),
    );
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

/// What a `<w:p>` turned into. List paragraphs are kept separate from plain
/// blocks because placing them needs the surrounding block list (to nest under
/// a parent item and to continue a run of items), which `paragraph_block`
/// cannot see.
enum ParagraphOutput {
    Block(Block),
    ListItem {
        level: u8,
        marker: ListMarker,
        ordinal: u64,
        content: Vec<Inline>,
    },
}

fn paragraph_block(
    paragraph: Paragraph,
    context: &DocxContext<'_>,
    counters: &mut ListCounters,
) -> Option<ParagraphOutput> {
    if paragraph.content.is_empty() {
        return None;
    }
    if let Some(level) = heading_level(paragraph.style.as_deref(), context.styles) {
        return Some(ParagraphOutput::Block(Block::Heading {
            level,
            content: paragraph.content,
        }));
    }
    if let Some((number_id, level)) = paragraph.numbering {
        let definition = context
            .numbering
            .get(&(number_id.clone(), level))
            .cloned()
            .unwrap_or(NumberDef {
                marker: ListMarker::Bullet,
                start: 1,
            });
        let ordinal = counters.next(&number_id, level, definition.start);
        let mut content = paragraph.content;
        // Word separates the generated number from the item text with a tab
        // stop. The number is regenerated by the renderer, so that tab is
        // layout residue, not content.
        trim_leading_whitespace(&mut content);
        return Some(ParagraphOutput::ListItem {
            level,
            marker: definition.marker,
            ordinal,
            content,
        });
    }
    Some(ParagraphOutput::Block(Block::Paragraph {
        content: paragraph.content,
    }))
}

fn trim_leading_whitespace(content: &mut Vec<Inline>) {
    while let Some(Inline::Text { text, .. }) = content.first_mut() {
        let trimmed = text.trim_start().to_owned();
        if trimmed.is_empty() {
            content.remove(0);
            continue;
        }
        *text = trimmed;
        break;
    }
}

/// Running position of every `(numId, level)` sequence in the document.
///
/// Word numbering continues across intervening body paragraphs, so the
/// ordinal cannot be derived from a list block's own item count — a list
/// interrupted by a paragraph and then resumed has to pick the counter back
/// up where it left off.
#[derive(Default)]
struct ListCounters {
    next: HashMap<(String, u8), u64>,
}

impl ListCounters {
    fn next(&mut self, number_id: &str, level: u8, start: u64) -> u64 {
        let ordinal = *self
            .next
            .entry((number_id.to_owned(), level))
            .or_insert(start);
        self.next.insert((number_id.to_owned(), level), ordinal + 1);
        // Entering a level restarts every deeper level beneath it.
        self.next
            .retain(|(id, other), _| id != number_id || *other <= level);
        ordinal
    }
}

/// Place a list item, nesting it under the enclosing item chain when its
/// level is deeper than the surrounding block list.
fn push_list_item(
    container: &mut Vec<Block>,
    level: u8,
    marker: ListMarker,
    ordinal: u64,
    content: Vec<Inline>,
) {
    if level > 0
        && let Some(Block::List { list }) = container.last_mut()
    {
        if list.items.is_empty() {
            list.items.push(ListItem { blocks: Vec::new() });
        }
        let inner = &mut list.items.last_mut().unwrap().blocks;
        push_list_item(inner, level - 1, marker, ordinal, content);
        return;
    }
    let item = ListItem {
        blocks: vec![Block::Paragraph { content }],
    };
    // Continue the preceding list only when this item is genuinely its next
    // element; otherwise a new sequence starts.
    if let Some(Block::List { list }) = container.last_mut()
        && list.marker == marker
        && list.start.unwrap_or(1) + list.items.len() as u64 == ordinal
    {
        list.items.push(item);
        return;
    }
    container.push(Block::List {
        list: List {
            marker,
            start: Some(ordinal),
            items: vec![item],
        },
    });
}

/// Collapse a style's `basedOn` chain into the run properties it implies.
///
/// The chain is walked base-first so a derived style's own properties win,
/// matching ECMA-376's cascade. A cycle terminates the walk rather than
/// hanging.
fn resolved_run_style(id: &str, styles: &HashMap<String, StyleDef>) -> Style {
    let mut chain = Vec::new();
    let mut visited = HashSet::new();
    let mut current = id.to_owned();
    for _ in 0..16 {
        if !visited.insert(current.clone()) {
            break;
        }
        let Some(definition) = styles.get(&current) else {
            break;
        };
        chain.push(definition);
        match definition.based_on.as_deref() {
            Some(parent) => current = parent.to_owned(),
            None => break,
        }
    }
    let mut style = Style::default();
    for definition in chain.into_iter().rev() {
        merge_run_style(&mut style, &definition.run_style);
    }
    style
}

/// Apply the set properties of `source` over `target`. `Style` has no
/// tri-state per property, so only enabled flags propagate — a derived style
/// cannot currently switch an inherited property back off.
fn merge_run_style(target: &mut Style, source: &Style) {
    target.bold |= source.bold;
    target.italic |= source.italic;
    target.underline |= source.underline;
    target.strike |= source.strike;
    target.code |= source.code;
    if source.superscript.is_some() {
        target.superscript = source.superscript;
    }
    if source.language.is_some() {
        target.language = source.language.clone();
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
        // A character style named "Heading 1 Char" must not turn its
        // paragraph into a heading.
        if !definition.is_paragraph_style {
            return None;
        }
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

fn place_paragraph_output(produced: ParagraphOutput, container: &mut Vec<Block>) {
    match produced {
        ParagraphOutput::Block(block) => container.push(block),
        ParagraphOutput::ListItem {
            level,
            marker,
            ordinal,
            content,
        } => push_list_item(container, level, marker, ordinal, content),
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
                // Character styles matter as much as paragraph styles here:
                // `<w:rStyle>` resolves against them, and dropping them was
                // why style-driven bold/italic/strike never reached the IR.
                let kind = attribute(&event, b"type");
                current_id = attribute(&event, b"styleId");
                current = StyleDef {
                    is_paragraph_style: kind.as_deref() == Some("paragraph"),
                    ..StyleDef::default()
                };
            }
            Ok(Event::Start(event) | Event::Empty(event)) if current_id.is_some() => {
                match event.local_name().as_ref() {
                    b"name" => current.name = attribute(&event, b"val"),
                    b"basedOn" => current.based_on = attribute(&event, b"val"),
                    b"outlineLvl" => {
                        current.outline_level =
                            attribute(&event, b"val").and_then(|value| value.parse().ok())
                    }
                    _ => update_run_style(&event, &mut current.run_style),
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

/// Guard against deeply nested XML, which turns recursive descent (and, for
/// the tolerant reader below, quick-xml's own bookkeeping) into a CPU/memory
/// amplifier for an attacker-controlled package.
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

#[cfg(test)]
mod rich_object_tests {
    use super::*;

    const CHART: &[u8] = br#"
        <c:chartSpace xmlns:c="c" xmlns:a="a">
          <c:chart>
            <c:title><c:tx><c:rich><a:p><a:r><a:t>Quarterly Widgets</a:t></a:r></a:p></c:rich></c:tx></c:title>
            <c:plotArea>
              <c:barChart><c:ser>
                <c:tx><c:strRef><c:strCache><c:pt><c:v>Widgets</c:v></c:pt></c:strCache></c:strRef></c:tx>
                <c:cat><c:strRef><c:strCache>
                  <c:pt><c:v>Q1</c:v></c:pt><c:pt><c:v>Q2</c:v></c:pt>
                </c:strCache></c:strRef></c:cat>
                <c:val><c:numRef><c:numCache>
                  <c:pt><c:v>10</c:v></c:pt><c:pt><c:v>14</c:v></c:pt>
                </c:numCache></c:numRef></c:val>
              </c:ser></c:barChart>
              <c:catAx><c:title><c:tx><c:rich><a:p><a:r><a:t>Quarter</a:t></a:r></a:p></c:rich></c:tx></c:title></c:catAx>
              <c:valAx><c:title><c:tx><c:rich><a:p><a:r><a:t>Units</a:t></a:r></a:p></c:rich></c:tx></c:title></c:valAx>
            </c:plotArea>
          </c:chart>
        </c:chartSpace>"#;

    #[test]
    fn chart_becomes_a_titled_data_table() {
        let blocks =
            parse_chart_xml(CHART, "word/charts/chart1.xml", &ParseOptions::default()).unwrap();
        assert!(matches!(
            &blocks[0],
            Block::Paragraph { content }
                if matches!(&content[0], Inline::Text { text, style }
                    if text == "Quarterly Widgets" && style.bold)
        ));
        let Block::Table { table } = &blocks[1] else {
            panic!("chart data was not represented as a table");
        };
        assert_eq!((table.rows, table.columns, table.header_rows), (3, 2, 1));
        assert!(matches!(
            &table.grid[0][0],
            CellSlot::Origin(Cell { blocks, .. }) if blocks == &vec![Block::paragraph("Quarter")]
        ));
        assert!(matches!(
            &table.grid[2][1],
            CellSlot::Origin(Cell { value_kind: CellValueKind::Number, blocks, .. })
                if blocks == &vec![Block::paragraph("14")]
        ));
    }

    #[test]
    fn chart_without_titles_or_aligned_categories_still_keeps_series_data() {
        let xml = br#"<c:chartSpace xmlns:c="c"><c:chart><c:plotArea><c:barChart>
          <c:ser><c:val><c:numRef><c:numCache><c:pt><c:v>7</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser>
          <c:ser>
            <c:tx><c:strRef><c:strCache><c:pt><c:v>Named</c:v></c:pt></c:strCache></c:strRef></c:tx>
            <c:cat><c:strRef><c:strCache><c:pt><c:v>Only category</c:v></c:pt></c:strCache></c:strRef></c:cat>
            <c:val><c:strRef><c:strCache><c:pt><c:v>N/A</c:v></c:pt></c:strCache></c:strRef></c:val>
          </c:ser>
        </c:barChart></c:plotArea></c:chart></c:chartSpace>"#;
        let blocks =
            parse_chart_xml(xml, "word/charts/chart2.xml", &ParseOptions::default()).unwrap();
        let [Block::Table { table }] = blocks.as_slice() else {
            panic!("untitled chart should still produce its data table");
        };
        assert_eq!((table.rows, table.columns), (2, 3));
        assert_eq!(
            table.grid[0],
            vec![
                CellSlot::Origin(Cell::text("Category", CellValueKind::Text)),
                CellSlot::Origin(Cell::text("Series 1", CellValueKind::Text)),
                CellSlot::Origin(Cell::text("Named", CellValueKind::Text)),
            ]
        );
        assert!(matches!(
            &table.grid[1][0],
            CellSlot::Origin(Cell { blocks, .. })
                if blocks == &vec![Block::paragraph("Only category")]
        ));
        assert!(matches!(
            &table.grid[1][1],
            CellSlot::Origin(Cell {
                value_kind: CellValueKind::Number,
                ..
            })
        ));
        assert!(matches!(
            &table.grid[1][2],
            CellSlot::Origin(Cell {
                value_kind: CellValueKind::Text,
                ..
            })
        ));
    }

    #[test]
    fn diagram_text_becomes_a_bullet_list() {
        let xml = br#"<dgm:dataModel xmlns:dgm="dgm" xmlns:a="a"><dgm:ptLst>
          <dgm:pt><dgm:t><a:p><a:r><a:t>Plan</a:t></a:r></a:p></dgm:t></dgm:pt>
          <dgm:pt><dgm:t><a:p><a:r><a:t>Build</a:t></a:r></a:p></dgm:t></dgm:pt>
          <dgm:pt><dgm:t><a:p><a:r><a:t>Ship</a:t></a:r></a:p></dgm:t></dgm:pt>
        </dgm:ptLst></dgm:dataModel>"#;
        let blocks =
            parse_diagram_xml(xml, "word/diagrams/data1.xml", &ParseOptions::default()).unwrap();
        let Block::List { list } = &blocks[0] else {
            panic!("diagram text was not represented as a list");
        };
        assert_eq!(list.marker, ListMarker::Bullet);
        assert_eq!(list.items.len(), 3);
        assert_eq!(list.items[2].blocks, vec![Block::paragraph("Ship")]);
    }

    #[test]
    fn empty_diagram_produces_no_placeholder_block() {
        let blocks = parse_diagram_xml(
            br#"<dgm:dataModel xmlns:dgm="dgm"/>"#,
            "word/diagrams/empty.xml",
            &ParseOptions::default(),
        )
        .unwrap();
        assert!(blocks.is_empty());
    }

    #[test]
    fn rich_object_parts_reject_malformed_xml_and_enforce_event_budgets() {
        let options = ParseOptions::default();
        assert!(matches!(
            parse_chart_xml(
                b"<c:ser><c:val><c:v>&bogus;</c:v></c:val></c:ser>",
                "chart.xml",
                &options,
            ),
            Err(DocumentError::Malformed { .. })
        ));
        assert!(matches!(
            parse_diagram_xml(b"<a:t>&bogus;</a:t>", "diagram.xml", &options),
            Err(DocumentError::Malformed { .. })
        ));

        let mut limited = ParseOptions::default();
        limited.limits.max_xml_nodes = 1;
        assert!(matches!(
            parse_chart_xml(b"<root/>", "chart.xml", &limited),
            Err(DocumentError::ResourceLimit {
                limit: "max_xml_nodes",
                ..
            })
        ));
        assert!(matches!(
            parse_diagram_xml(b"<root/>", "diagram.xml", &limited),
            Err(DocumentError::ResourceLimit {
                limit: "max_xml_nodes",
                ..
            })
        ));
    }

    #[test]
    fn document_binds_rich_objects_alt_text_and_ole_description() {
        let xml = br#"<w:document xmlns:w="w" xmlns:r="r" xmlns:a="a" xmlns:c="c" xmlns:dgm="dgm" xmlns:wp="wp" xmlns:o="o">
          <w:body>
            <w:p><w:r><w:drawing><wp:docPr descr="tiny dot image"/><a:blip r:embed="rId30"/></w:drawing></w:r></w:p>
            <w:p><w:r><w:drawing><c:chart r:id="rId10"/></w:drawing></w:r></w:p>
            <w:p><w:r><w:drawing><dgm:relIds r:dm="rId20"/></w:drawing></w:r></w:p>
            <w:p><w:r><w:object><o:OLEObject ProgID="Excel.Sheet.12"/></w:object></w:r></w:p>
          </w:body>
        </w:document>"#;
        let styles = HashMap::new();
        let numbering = HashMap::new();
        let relationships = HashMap::new();
        let image_ids = HashMap::from([("rId30".to_owned(), "asset-1".to_owned())]);
        let related_blocks = HashMap::from([
            ("rId10".to_owned(), vec![Block::paragraph("chart content")]),
            (
                "rId20".to_owned(),
                vec![Block::paragraph("diagram content")],
            ),
        ]);
        let context = DocxContext {
            styles: &styles,
            numbering: &numbering,
            relationships: &relationships,
            image_ids: &image_ids,
            related_blocks: &related_blocks,
        };
        let mut output = Vec::new();
        parse_document_xml(
            xml,
            DOCUMENT_PART,
            &mut output,
            &mut Vec::new(),
            &ParseOptions::default(),
            &context,
        )
        .unwrap();

        assert!(matches!(
            &output[0],
            Block::Paragraph { content }
                if matches!(&content[0], Inline::Image { alt: Some(alt), .. }
                    if alt == "tiny dot image")
        ));
        assert_eq!(output[1], Block::paragraph("chart content"));
        assert_eq!(output[2], Block::paragraph("diagram content"));
        assert_eq!(
            output[3],
            Block::paragraph("Embedded object: Excel.Sheet.12")
        );
    }
}
