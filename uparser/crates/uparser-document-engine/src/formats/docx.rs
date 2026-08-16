use crate::ooxml::{
    REL_ENDNOTES, REL_FOOTER, REL_FOOTNOTES, REL_HEADER, REL_NUMBERING, REL_STYLES,
    ROOT_RELATIONSHIPS_PART, Relationships, attribute, load_image_relationships,
    load_relationships, load_root_relationships, main_part, related_part,
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
    let context = DocxContext {
        styles: &styles,
        numbering: &numbering,
        relationships: &relationships,
        image_ids: &image_ids,
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
                    b"p" => paragraph = Some(Paragraph::default()),
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
                        if let Some(paragraph) = paragraph.take()
                            && let Some(produced) =
                                paragraph_block(paragraph, context, &mut counters)
                        {
                            let container =
                                match table.as_mut().and_then(|table| table.cell.as_mut()) {
                                    Some(cell) => &mut cell.blocks,
                                    None => &mut *output,
                                };
                            place_paragraph_output(produced, container);
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
