use crate::ooxml::{attribute, resolve_internal_target};
use crate::package::Package;
use crate::{
    Asset, AssetId, Block, CanonicalDocument, Cell, CellSlot, CellValueKind, DocumentError,
    DocumentFormat, DocumentUnit, FormulaSource, ImageSource, Inline, LinkTarget, List, ListItem,
    ListMarker, Note, NoteKind, ParseOptions, ParseWarning, Style, Table, TableKind, UnitKind,
    WarningCode,
};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

const CONTAINER_PART: &str = "META-INF/container.xml";

pub(crate) fn parse(
    bytes: &[u8],
    options: &ParseOptions,
) -> Result<CanonicalDocument, DocumentError> {
    let mut package = Package::open(bytes, &options.limits)?;
    let container = package.read_required(CONTAINER_PART)?;
    let opf_part = parse_container(&container, options)?;
    let opf_xml = package.read_required(&opf_part)?;
    let publication = parse_opf(&opf_xml, &opf_part, options)?;

    let mut document = CanonicalDocument::new(DocumentFormat::Epub);
    document.metadata.variant = Some("epub".to_owned());
    document.metadata.title = publication.title;
    document.metadata.author = publication.author;
    document.metadata.language = publication.language;
    if let Some(navigation) = load_navigation(&mut package, &opf_part, &publication, options)? {
        document
            .metadata
            .properties
            .insert("epub.navigation".to_owned(), navigation);
    }

    let mut asset_ids = HashMap::new();
    for item in publication
        .manifest
        .values()
        .filter(|item| item.media_type.starts_with("image/"))
    {
        let Some(part) = resolve_href(&opf_part, &item.href) else {
            document.warnings.push(broken(&opf_part, &item.href));
            continue;
        };
        let Some(asset_bytes) = package.read(&part)? else {
            document.warnings.push(broken(&opf_part, &part));
            continue;
        };
        if asset_bytes.len() > options.limits.max_asset_bytes {
            document.warnings.push(ParseWarning {
                code: WarningCode::AssetDropped,
                part: Some(part),
                message: format!(
                    "EPUB image exceeds {} bytes",
                    options.limits.max_asset_bytes
                ),
            });
            continue;
        }
        let sha256 = format!("{:x}", Sha256::digest(&asset_bytes));
        let id = format!("asset-{}", &sha256[..16]);
        asset_ids.insert(part.clone(), id.clone());
        if !document.assets.iter().any(|asset| asset.id == id) {
            document.assets.push(Asset {
                id,
                media_type: item.media_type.clone(),
                filename: part.rsplit('/').next().map(ToOwned::to_owned),
                byte_length: asset_bytes.len(),
                sha256,
                bytes: options.include_assets.then_some(asset_bytes),
            });
        }
    }

    for idref in publication.spine {
        let Some(item) = publication.manifest.get(&idref) else {
            document.warnings.push(ParseWarning {
                code: WarningCode::BrokenRelationship,
                part: Some(opf_part.clone()),
                message: format!("EPUB spine references missing manifest item {idref:?}"),
            });
            continue;
        };
        let Some(part) = resolve_href(&opf_part, &item.href) else {
            document.warnings.push(broken(&opf_part, &item.href));
            continue;
        };
        let Some(xhtml) = package.read(&part)? else {
            document.warnings.push(broken(&opf_part, &part));
            continue;
        };
        let (notes, note_ids) = extract_notes(&xhtml, &part, options)?;
        document.notes.extend(notes);
        let mut unit = parse_xhtml(
            &xhtml,
            &part,
            document.units.len(),
            options,
            &asset_ids,
            &note_ids,
            &mut document.warnings,
        )?;
        if unit.label.is_none() {
            unit.label = Some(idref);
        }
        document.units.push(unit);
    }
    Ok(document)
}

fn parse_container(xml: &[u8], options: &ParseOptions) -> Result<String, DocumentError> {
    let mut reader = Reader::from_reader(xml);
    let mut nodes = 0usize;
    loop {
        nodes += 1;
        enforce_nodes(nodes, CONTAINER_PART, options)?;
        match reader.read_event() {
            Ok(Event::Start(event) | Event::Empty(event))
                if event.local_name().as_ref() == b"rootfile" =>
            {
                if let Some(path) = attribute(&event, b"full-path") {
                    return resolve_internal_target("", &path).ok_or_else(|| {
                        DocumentError::Malformed {
                            part: Some(CONTAINER_PART.to_owned()),
                            detail: "EPUB rootfile escapes the package".to_owned(),
                        }
                    });
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(malformed(CONTAINER_PART, error)),
            _ => {}
        }
    }
    Err(DocumentError::MissingPart {
        part: "EPUB rootfile declaration".to_owned(),
    })
}

#[derive(Clone)]
struct ManifestItem {
    href: String,
    media_type: String,
    properties: String,
}

#[derive(Default)]
struct Publication {
    title: Option<String>,
    author: Option<String>,
    language: Option<String>,
    manifest: HashMap<String, ManifestItem>,
    spine: Vec<String>,
    toc: Option<String>,
}

fn parse_opf(xml: &[u8], part: &str, options: &ParseOptions) -> Result<Publication, DocumentError> {
    let mut reader = Reader::from_reader(xml);
    reader.trim_text(true);
    let mut publication = Publication::default();
    let mut metadata_field: Option<Vec<u8>> = None;
    let mut nodes = 0usize;
    loop {
        nodes += 1;
        enforce_nodes(nodes, part, options)?;
        match reader.read_event() {
            Ok(Event::Start(event)) => match event.local_name().as_ref() {
                b"title" | b"creator" | b"language" => {
                    metadata_field = Some(event.local_name().as_ref().to_vec())
                }
                b"item" => insert_manifest(&event, &mut publication.manifest),
                b"itemref" => insert_spine(&event, &mut publication.spine),
                b"spine" => publication.toc = attribute(&event, b"toc"),
                _ => {}
            },
            Ok(Event::Empty(event)) => match event.local_name().as_ref() {
                b"item" => insert_manifest(&event, &mut publication.manifest),
                b"itemref" => insert_spine(&event, &mut publication.spine),
                _ => {}
            },
            Ok(Event::Text(text)) => {
                if let Some(field) = metadata_field.as_deref() {
                    let value = text
                        .unescape()
                        .map_err(|error| malformed(part, error))?
                        .into_owned();
                    match field {
                        b"title" if publication.title.is_none() => publication.title = Some(value),
                        b"creator" if publication.author.is_none() => {
                            publication.author = Some(value)
                        }
                        b"language" if publication.language.is_none() => {
                            publication.language = Some(value)
                        }
                        _ => {}
                    }
                }
            }
            Ok(Event::End(event))
                if matches!(
                    event.local_name().as_ref(),
                    b"title" | b"creator" | b"language"
                ) =>
            {
                metadata_field = None
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(malformed(part, error)),
            _ => {}
        }
    }
    Ok(publication)
}

fn insert_manifest(event: &BytesStart<'_>, manifest: &mut HashMap<String, ManifestItem>) {
    if let (Some(id), Some(href)) = (attribute(event, b"id"), attribute(event, b"href")) {
        manifest.insert(
            id,
            ManifestItem {
                href,
                media_type: attribute(event, b"media-type")
                    .unwrap_or_else(|| "application/octet-stream".to_owned()),
                properties: attribute(event, b"properties").unwrap_or_default(),
            },
        );
    }
}

fn insert_spine(event: &BytesStart<'_>, spine: &mut Vec<String>) {
    if let Some(idref) = attribute(event, b"idref") {
        spine.push(idref);
    }
}

fn load_navigation(
    package: &mut Package<'_>,
    opf_part: &str,
    publication: &Publication,
    options: &ParseOptions,
) -> Result<Option<String>, DocumentError> {
    let item = publication
        .manifest
        .values()
        .find(|item| item.properties.split_whitespace().any(|value| value == "nav"))
        .or_else(|| {
            publication
                .toc
                .as_ref()
                .and_then(|id| publication.manifest.get(id))
        })
        .or_else(|| {
            publication
                .manifest
                .values()
                .find(|item| item.media_type == "application/x-dtbncx+xml")
        });
    let Some(item) = item else {
        return Ok(None);
    };
    let Some(part) = resolve_href(opf_part, &item.href) else {
        return Ok(None);
    };
    let Some(xml) = package.read(&part)? else {
        return Ok(None);
    };
    let entries = if item.media_type == "application/x-dtbncx+xml" {
        parse_ncx_navigation(&xml, &part, options)?
    } else {
        parse_xhtml_navigation(&xml, &part, options)?
    };
    (!entries.is_empty())
        .then(|| serde_json::to_string(&entries).map_err(|error| DocumentError::malformed(error.to_string())))
        .transpose()
}

fn parse_xhtml_navigation(
    xml: &[u8],
    part: &str,
    options: &ParseOptions,
) -> Result<Vec<(String, String)>, DocumentError> {
    let mut reader = Reader::from_reader(xml);
    let mut active: Option<(String, String)> = None;
    let mut entries = Vec::new();
    let mut nodes = 0usize;
    loop {
        nodes += 1;
        enforce_nodes(nodes, part, options)?;
        match reader.read_event() {
            Ok(Event::Start(event)) if event.local_name().as_ref() == b"a" => {
                active = attribute(&event, b"href").map(|href| (href, String::new()));
            }
            Ok(Event::Text(text)) => {
                if let Some((_, label)) = active.as_mut() {
                    label.push_str(&text.unescape().map_err(|error| malformed(part, error))?);
                }
            }
            Ok(Event::End(event)) if event.local_name().as_ref() == b"a" => {
                if let Some((href, label)) = active.take()
                    && !label.trim().is_empty()
                {
                    entries.push((label.trim().to_owned(), href));
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(malformed(part, error)),
            _ => {}
        }
    }
    Ok(entries)
}

fn parse_ncx_navigation(
    xml: &[u8],
    part: &str,
    options: &ParseOptions,
) -> Result<Vec<(String, String)>, DocumentError> {
    let mut reader = Reader::from_reader(xml);
    let mut in_label = false;
    let mut label = String::new();
    let mut entries = Vec::new();
    let mut nodes = 0usize;
    loop {
        nodes += 1;
        enforce_nodes(nodes, part, options)?;
        match reader.read_event() {
            Ok(Event::Start(event)) if event.local_name().as_ref() == b"navLabel" => {
                in_label = true;
                label.clear();
            }
            Ok(Event::Text(text)) if in_label => {
                label.push_str(&text.unescape().map_err(|error| malformed(part, error))?);
            }
            Ok(Event::End(event)) if event.local_name().as_ref() == b"navLabel" => {
                in_label = false;
            }
            Ok(Event::Start(event) | Event::Empty(event))
                if event.local_name().as_ref() == b"content" =>
            {
                if let Some(src) = attribute(&event, b"src")
                    && !label.trim().is_empty()
                {
                    entries.push((label.trim().to_owned(), src));
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(malformed(part, error)),
            _ => {}
        }
    }
    Ok(entries)
}

#[derive(Default)]
struct Paragraph {
    heading: Option<u8>,
    content: Vec<Inline>,
    link: Option<Link>,
}

struct Link {
    target: LinkTarget,
    content: Vec<Inline>,
    note_id: Option<String>,
}

struct ListBuilder {
    marker: ListMarker,
    items: Vec<ListItem>,
}

#[derive(Default)]
struct RawCell {
    blocks: Vec<Block>,
    row_span: usize,
    column_span: usize,
    header: bool,
}

#[derive(Default)]
struct TableBuilder {
    rows: Vec<Vec<RawCell>>,
    row: Option<Vec<RawCell>>,
    cell: Option<RawCell>,
}

struct XhtmlState {
    unit: DocumentUnit,
    paragraph: Option<Paragraph>,
    lists: Vec<ListBuilder>,
    table: Option<TableBuilder>,
    quote: Option<Vec<Block>>,
    style: Style,
    style_stack: Vec<Style>,
    in_body: bool,
    ignore_depth: usize,
    pre: Option<String>,
    depth: usize,
    nodes: usize,
    semantic_skip_depth: usize,
    math: Option<(usize, String)>,
}

fn extract_notes(
    xml: &[u8],
    part: &str,
    options: &ParseOptions,
) -> Result<(Vec<Note>, HashMap<String, String>), DocumentError> {
    if !options.include_notes {
        return Ok((Vec::new(), HashMap::new()));
    }
    let mut reader = Reader::from_reader(xml);
    reader.trim_text(false);
    let mut active: Option<(String, NoteKind, usize, String)> = None;
    let mut notes = Vec::new();
    let mut ids = HashMap::new();
    let mut nodes = 0usize;
    loop {
        nodes += 1;
        enforce_nodes(nodes, part, options)?;
        match reader.read_event() {
            Ok(Event::Start(event)) => {
                if let Some((_, _, depth, _)) = active.as_mut() {
                    *depth += 1;
                } else if let (Some(id), Some(kind)) =
                    (attribute(&event, b"id"), attribute(&event, b"type"))
                    && let Some(note_kind) = semantic_note_kind(&kind)
                {
                    active = Some((id, note_kind, 1, String::new()));
                }
            }
            Ok(Event::Text(text)) => {
                if let Some((_, _, _, content)) = active.as_mut() {
                    content.push_str(&text.unescape().map_err(|error| malformed(part, error))?);
                }
            }
            Ok(Event::End(_)) => {
                if let Some((_, _, depth, _)) = active.as_mut() {
                    *depth -= 1;
                    if *depth == 0 {
                        let (source_id, kind, _, content) = active.take().unwrap();
                        let id = format!("{part}#{source_id}");
                        ids.insert(source_id, id.clone());
                        notes.push(Note {
                            id,
                            kind,
                            blocks: (!content.trim().is_empty())
                                .then(|| vec![Block::paragraph(content.trim())])
                                .unwrap_or_default(),
                        });
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(malformed(part, error)),
            _ => {}
        }
    }
    Ok((notes, ids))
}

fn semantic_note_kind(value: &str) -> Option<NoteKind> {
    if value.split_whitespace().any(|item| item == "footnote") {
        Some(NoteKind::Footnote)
    } else if value.split_whitespace().any(|item| item == "endnote") {
        Some(NoteKind::Endnote)
    } else {
        None
    }
}

fn parse_xhtml(
    xml: &[u8],
    part: &str,
    index: usize,
    options: &ParseOptions,
    asset_ids: &HashMap<String, AssetId>,
    note_ids: &HashMap<String, String>,
    warnings: &mut Vec<ParseWarning>,
) -> Result<DocumentUnit, DocumentError> {
    let mut state = XhtmlState {
        unit: DocumentUnit::new(UnitKind::Chapter, index, None),
        paragraph: None,
        lists: Vec::new(),
        table: None,
        quote: None,
        style: Style::default(),
        style_stack: Vec::new(),
        in_body: false,
        ignore_depth: 0,
        pre: None,
        depth: 0,
        nodes: 0,
        semantic_skip_depth: 0,
        math: None,
    };
    let mut reader = Reader::from_reader(xml);
    reader.trim_text(false);
    loop {
        let event = reader
            .read_event()
            .map_err(|error| malformed(part, error))?;
        state.nodes += 1;
        enforce_nodes(state.nodes, part, options)?;
        match event {
            Event::Start(event) => {
                state.depth += 1;
                if state.depth > options.limits.max_xml_depth {
                    return Err(DocumentError::ResourceLimit {
                        limit: "max_xml_depth",
                        detail: format!("{part} exceeds XHTML nesting depth"),
                    });
                }
                xhtml_start(&event, part, asset_ids, note_ids, &mut state);
            }
            Event::Empty(event) => {
                xhtml_start(&event, part, asset_ids, note_ids, &mut state);
                xhtml_end(event.local_name().as_ref(), &mut state, warnings, options)?;
            }
            Event::Text(text) if state.math.is_some() => {
                let value = text.unescape().map_err(|error| malformed(part, error))?;
                state.math.as_mut().unwrap().1.push_str(&value);
            }
            Event::Text(text)
                if state.ignore_depth == 0 && state.semantic_skip_depth == 0 && state.in_body =>
            {
                let value = text.unescape().map_err(|error| malformed(part, error))?;
                if let Some(pre) = state.pre.as_mut() {
                    pre.push_str(&value);
                } else if let Some(paragraph) = state.paragraph.as_mut() {
                    push_inline(
                        paragraph,
                        Inline::Text {
                            text: value.into_owned(),
                            style: state.style.clone(),
                        },
                    );
                }
            }
            Event::End(event) => {
                xhtml_end(event.local_name().as_ref(), &mut state, warnings, options)?;
                state.depth = state.depth.saturating_sub(1);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if state.unit.label.is_none() {
        state.unit.label = state.unit.blocks.iter().find_map(|block| match block {
            Block::Heading { content, .. } => Some(inline_text(content)),
            _ => None,
        });
    }
    Ok(state.unit)
}

fn xhtml_start(
    event: &BytesStart<'_>,
    part: &str,
    asset_ids: &HashMap<String, AssetId>,
    note_ids: &HashMap<String, String>,
    state: &mut XhtmlState,
) {
    let name = event.local_name();
    let name = name.as_ref();
    if let Some((depth, _)) = state.math.as_mut() {
        *depth += 1;
        return;
    }
    if name == b"math" {
        state.math = Some((1, String::new()));
        return;
    }
    if state.semantic_skip_depth > 0 {
        state.semantic_skip_depth += 1;
        return;
    }
    if attribute(event, b"id").is_some_and(|id| note_ids.contains_key(&id)) {
        state.semantic_skip_depth = 1;
        return;
    }
    if matches!(name, b"script" | b"style") {
        state.ignore_depth += 1;
        return;
    }
    if state.ignore_depth > 0 {
        return;
    }
    match name {
        b"body" => state.in_body = true,
        b"h1" | b"h2" | b"h3" | b"h4" | b"h5" | b"h6" => {
            state.paragraph = Some(Paragraph {
                heading: Some((name[1] - b'0').clamp(1, 6)),
                ..Default::default()
            });
            push_anchor(event, state);
        }
        b"p" => {
            state.paragraph = Some(Paragraph::default());
            push_anchor(event, state);
        }
        b"a" => {
            if let (Some(paragraph), Some(href)) =
                (state.paragraph.as_mut(), attribute(event, b"href"))
            {
                let target = if let Some(anchor) = href.strip_prefix('#') {
                    LinkTarget::Anchor(anchor.to_owned())
                } else {
                    LinkTarget::External(href.clone())
                };
                paragraph.link = Some(Link {
                    target,
                    content: Vec::new(),
                    note_id: attribute(event, b"type")
                        .is_some_and(|kind| kind.split_whitespace().any(|item| item == "noteref"))
                        .then(|| href.strip_prefix('#'))
                        .flatten()
                        .and_then(|id| note_ids.get(id))
                        .cloned(),
                });
            }
        }
        b"br" => {
            if let Some(paragraph) = state.paragraph.as_mut() {
                push_inline(paragraph, Inline::LineBreak);
            }
        }
        b"img" => {
            let src = attribute(event, b"src").unwrap_or_default();
            let resolved = resolve_href(part, &src);
            let asset_id = resolved
                .as_ref()
                .and_then(|path| asset_ids.get(path))
                .cloned();
            let source = asset_id
                .clone()
                .map(ImageSource::Asset)
                .unwrap_or_else(|| ImageSource::External(src));
            let alt = attribute(event, b"alt");
            if let Some(paragraph) = state.paragraph.as_mut() {
                push_inline(paragraph, Inline::Image { source, alt });
            } else {
                append(
                    Block::Figure {
                        asset_id,
                        alt,
                        caption: Vec::new(),
                    },
                    state,
                );
            }
        }
        b"ul" | b"ol" => state.lists.push(ListBuilder {
            marker: if name == b"ol" {
                ListMarker::Decimal
            } else {
                ListMarker::Bullet
            },
            items: Vec::new(),
        }),
        b"li" => {
            if let Some(list) = state.lists.last_mut() {
                list.items.push(ListItem { blocks: Vec::new() });
            }
            if state.paragraph.is_none() {
                state.paragraph = Some(Paragraph::default());
            }
        }
        b"blockquote" => state.quote = Some(Vec::new()),
        b"pre" => state.pre = Some(String::new()),
        b"table" if state.table.is_none() => state.table = Some(TableBuilder::default()),
        b"tr" => {
            if let Some(table) = state.table.as_mut() {
                table.row = Some(Vec::new());
            }
        }
        b"td" | b"th" => {
            if let Some(table) = state.table.as_mut() {
                table.cell = Some(RawCell {
                    row_span: html_span(event, b"rowspan"),
                    column_span: html_span(event, b"colspan"),
                    header: name == b"th",
                    ..Default::default()
                });
            }
        }
        b"strong" | b"b" | b"em" | b"i" | b"u" | b"s" | b"del" | b"code" | b"sup" | b"sub" => {
            state.style_stack.push(state.style.clone());
            match name {
                b"strong" | b"b" => state.style.bold = true,
                b"em" | b"i" => state.style.italic = true,
                b"u" => state.style.underline = true,
                b"s" | b"del" => state.style.strike = true,
                b"code" => state.style.code = true,
                b"sup" => state.style.superscript = Some(true),
                b"sub" => state.style.superscript = Some(false),
                _ => {}
            }
        }
        _ => push_anchor(event, state),
    }
}

fn xhtml_end(
    name: &[u8],
    state: &mut XhtmlState,
    warnings: &mut Vec<ParseWarning>,
    options: &ParseOptions,
) -> Result<(), DocumentError> {
    if let Some((depth, _)) = state.math.as_mut() {
        if *depth > 1 {
            *depth -= 1;
        } else if name == b"math" {
            let (_, source) = state.math.take().unwrap();
            if let Some(paragraph) = state.paragraph.as_mut() {
                push_inline(
                    paragraph,
                    Inline::Formula {
                        source: FormulaSource::MathMl(source.clone()),
                        display: (!source.trim().is_empty()).then(|| source.trim().to_owned()),
                    },
                );
            }
        }
        return Ok(());
    }
    if state.semantic_skip_depth > 0 {
        state.semantic_skip_depth -= 1;
        return Ok(());
    }
    if matches!(name, b"script" | b"style") {
        state.ignore_depth = state.ignore_depth.saturating_sub(1);
        return Ok(());
    }
    if state.ignore_depth > 0 {
        return Ok(());
    }
    match name {
        b"body" => state.in_body = false,
        b"a" => {
            if let Some(paragraph) = state.paragraph.as_mut()
                && let Some(link) = paragraph.link.take()
            {
                if let Some(id) = link.note_id {
                    paragraph.content.push(Inline::NoteRef { id });
                } else {
                    paragraph.content.push(Inline::Link {
                        target: link.target,
                        content: link.content,
                    });
                }
            }
        }
        b"p" | b"h1" | b"h2" | b"h3" | b"h4" | b"h5" | b"h6" => {
            if let Some(paragraph) = state.paragraph.take()
                && !paragraph.content.is_empty()
            {
                let block = match paragraph.heading {
                    Some(level) => Block::Heading {
                        level,
                        content: paragraph.content,
                    },
                    None => Block::Paragraph {
                        content: paragraph.content,
                    },
                };
                append(block, state);
            }
        }
        b"ul" | b"ol" => {
            if let Some(list) = state.lists.pop() {
                append(
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
        b"li" => {
            if let Some(paragraph) = state.paragraph.take()
                && !paragraph.content.is_empty()
            {
                append(
                    Block::Paragraph {
                        content: paragraph.content,
                    },
                    state,
                );
            }
        }
        b"blockquote" => {
            if let Some(blocks) = state.quote.take() {
                append(Block::BlockQuote { blocks }, state);
            }
        }
        b"pre" => {
            if let Some(text) = state.pre.take() {
                append(
                    Block::CodeBlock {
                        language: None,
                        text,
                    },
                    state,
                );
            }
        }
        b"td" | b"th" => {
            if let Some(table) = state.table.as_mut()
                && let (Some(row), Some(cell)) = (table.row.as_mut(), table.cell.take())
            {
                row.push(cell);
            }
        }
        b"tr" => {
            if let Some(table) = state.table.as_mut()
                && let Some(row) = table.row.take()
            {
                table.rows.push(row);
            }
        }
        b"table" => {
            if let Some(table) = state.table.take() {
                append(
                    Block::Table {
                        table: build_html_table(table, warnings, options)?,
                    },
                    state,
                );
            }
        }
        b"strong" | b"b" | b"em" | b"i" | b"u" | b"s" | b"del" | b"code" | b"sup" | b"sub" => {
            state.style = state.style_stack.pop().unwrap_or_default();
        }
        _ => {}
    }
    Ok(())
}

fn append(block: Block, state: &mut XhtmlState) {
    if let Some(list) = state.lists.last_mut() {
        if list.items.is_empty() {
            list.items.push(ListItem { blocks: Vec::new() });
        }
        list.items.last_mut().unwrap().blocks.push(block);
    } else if let Some(cell) = state.table.as_mut().and_then(|table| table.cell.as_mut()) {
        cell.blocks.push(block);
    } else if let Some(quote) = state.quote.as_mut() {
        quote.push(block);
    } else {
        state.unit.blocks.push(block);
    }
}

fn push_inline(paragraph: &mut Paragraph, inline: Inline) {
    if let Some(link) = paragraph.link.as_mut() {
        link.content.push(inline);
    } else {
        paragraph.content.push(inline);
    }
}

fn push_anchor(event: &BytesStart<'_>, state: &mut XhtmlState) {
    if let (Some(paragraph), Some(id)) = (state.paragraph.as_mut(), attribute(event, b"id")) {
        push_inline(paragraph, Inline::Anchor { id });
    }
}

fn html_span(event: &BytesStart<'_>, name: &[u8]) -> usize {
    attribute(event, name)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1)
        .max(1)
}

fn build_html_table(
    raw: TableBuilder,
    warnings: &mut Vec<ParseWarning>,
    options: &ParseOptions,
) -> Result<Table, DocumentError> {
    let rows = raw.rows.len();
    let columns = raw
        .rows
        .iter()
        .map(|row| row.iter().map(|cell| cell.column_span).sum())
        .max()
        .unwrap_or(0);
    if (rows as u64).saturating_mul(columns as u64) > options.limits.max_expansion {
        return Err(DocumentError::ResourceLimit {
            limit: "max_expansion",
            detail: format!("EPUB table expands to {rows}x{columns} cells"),
        });
    }
    let mut header_rows = 0usize;
    let mut grid: Vec<Vec<Option<CellSlot>>> = vec![vec![None; columns]; rows];
    for (row_index, row) in raw.rows.into_iter().enumerate() {
        if !row.is_empty() && row.iter().all(|cell| cell.header) {
            header_rows += 1;
        }
        let mut column = 0usize;
        for cell in row {
            while column < columns && grid[row_index][column].is_some() {
                column += 1;
            }
            if column >= columns {
                break;
            }
            let row_span = cell.row_span.min(rows - row_index).max(1);
            let column_span = cell.column_span.min(columns - column).max(1);
            if row_span != cell.row_span || column_span != cell.column_span {
                warnings.push(ParseWarning {
                    code: WarningCode::InvalidSpanClamped,
                    part: None,
                    message: "EPUB table span exceeded the available grid and was clamped"
                        .to_owned(),
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
    Ok(Table {
        kind: TableKind::Data,
        rows,
        columns,
        header_rows,
        grid: grid
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .map(|slot| {
                        slot.unwrap_or_else(|| {
                            CellSlot::Origin(Cell::text("", CellValueKind::Empty))
                        })
                    })
                    .collect()
            })
            .collect(),
        caption: None,
    })
}

fn inline_text(content: &[Inline]) -> String {
    content
        .iter()
        .filter_map(|inline| match inline {
            Inline::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn resolve_href(source_part: &str, href: &str) -> Option<String> {
    let href = href
        .split('#')
        .next()
        .unwrap_or_default()
        .replace("%20", " ");
    resolve_internal_target(source_part, &href)
}

fn broken(part: &str, target: &str) -> ParseWarning {
    ParseWarning {
        code: WarningCode::BrokenRelationship,
        part: Some(part.to_owned()),
        message: format!("EPUB resource {target:?} is missing or escapes the package"),
    }
}

fn enforce_nodes(nodes: usize, part: &str, options: &ParseOptions) -> Result<(), DocumentError> {
    if nodes > options.limits.max_xml_nodes {
        return Err(DocumentError::ResourceLimit {
            limit: "max_xml_nodes",
            detail: format!("{part} contains too many XML events"),
        });
    }
    Ok(())
}

fn malformed(part: &str, error: impl std::fmt::Display) -> DocumentError {
    DocumentError::Malformed {
        part: Some(part.to_owned()),
        detail: error.to_string(),
    }
}
