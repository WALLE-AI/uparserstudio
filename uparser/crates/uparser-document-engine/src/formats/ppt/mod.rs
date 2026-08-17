//! Legacy binary PowerPoint (`.ppt`, MS-PPT).
//!
//! Same scope as the `.doc` reader: recover slide text, speaker notes and
//! slide order without LibreOffice, degrading unimplemented corners to plain
//! text rather than to failure.
//!
//! MS-PPT stores everything as a tree of records inside one OLE2 stream, and
//! — unlike a ZIP-based format — the tree's *stream order is not the
//! presentation order*. Editing reorders nothing on disk; the running order
//! lives in the document's `SlideListWithText`, and the byte offset of each
//! slide lives in a `PersistDirectoryAtom`. Both are followed here, because
//! taking stream order instead silently reorders any deck that has ever had a
//! slide moved.
//!
//! Formatting comes from two places at once, and both are needed: a slide's
//! own `StyleTextPropAtom` carries only the properties that *differ* from its
//! master, so a deck whose bullets and emphasis are entirely template-driven
//! carries no local styling at all. The master's `TxMasterStyleAtom` supplies
//! the per-indent-level defaults those local runs resolve against
//! (`styletext`), and each slide picks its master through
//! `SlideAtom.masterIdRef`.

mod pictures;
mod styletext;

use crate::{
    Asset, AssetId, Block, CanonicalDocument, DocumentError, DocumentFormat, DocumentUnit, Inline,
    List, ListItem, ListMarker, Note, NoteKind, ParseOptions, ParseWarning, Style, UnitKind,
    WarningCode,
};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Cursor, Read};
use styletext::{MasterLevel, MasterLevels, StyleRuns};

// Record types (MS-PPT §2.13.24).
const RT_DOCUMENT: u16 = 0x03E8;
const RT_SLIDE: u16 = 0x03EE;
const RT_SLIDE_ATOM: u16 = 0x03EF;
const RT_NOTES: u16 = 0x03F0;
const RT_NOTES_ATOM: u16 = 0x03F1;
const RT_SLIDE_PERSIST_ATOM: u16 = 0x03F3;
const RT_MAIN_MASTER: u16 = 0x03F8;
const RT_SLIDE_LIST_WITH_TEXT: u16 = 0x0FF0;
const RT_TEXT_HEADER_ATOM: u16 = 0x0F9F;
const RT_TEXT_CHARS_ATOM: u16 = 0x0FA0;
const RT_STYLE_TEXT_PROP_ATOM: u16 = 0x0FA1;
const RT_TX_MASTER_STYLE_ATOM: u16 = 0x0FA3;
const RT_TEXT_BYTES_ATOM: u16 = 0x0FA8;
const RT_PERSIST_DIRECTORY_ATOM: u16 = 0x1772;

// OfficeArt (MS-ODRAW) records, which carry the deck's pictures.
const OFFICE_ART_BSTORE: u16 = 0xF001;
const OFFICE_ART_FBSE: u16 = 0xF007;
const OFFICE_ART_FOPT: u16 = 0xF00B;

/// A record whose version nibble is 0xF contains other records rather than
/// data (MS-PPT §2.3.1).
const CONTAINER_VERSION: u16 = 0xF;

/// `SlideListWithText` appears up to three times in the document container,
/// distinguished only by its `recInstance` (MS-PPT §2.4.14.3). They all hold
/// `SlidePersistAtom`-shaped children, so reading them without checking the
/// instance mixes masters and notes pages into the slide order.
const LIST_INSTANCE_SLIDES: u16 = 0;
const LIST_INSTANCE_MASTERS: u16 = 1;

/// PowerPoint's own paragraph separator inside a text atom.
const PARAGRAPH_MARK: char = '\r';
const LINE_BREAK: char = '\u{b}';

/// Outline depth is capped before it reaches list building: the depth field
/// is a `u16`, and list nesting is built by recursion, so a corrupt atom
/// claiming depth 60000 would otherwise recurse that far.
const MAX_LIST_DEPTH: u16 = 9;

pub(crate) fn parse(
    bytes: &[u8],
    options: &ParseOptions,
) -> Result<CanonicalDocument, DocumentError> {
    let mut compound = cfb::CompoundFile::open(Cursor::new(bytes))
        .map_err(|error| DocumentError::malformed(format!("not an OLE2 container: {error}")))?;

    let stream = read_stream(&mut compound, "/PowerPoint Document", options)?.ok_or_else(|| {
        DocumentError::MissingPart {
            part: "PowerPoint Document".to_owned(),
        }
    })?;

    let mut document = CanonicalDocument::new(DocumentFormat::Ppt);
    document.metadata.variant = Some("ppt".to_owned());

    let mut budget = Budget::new(options);
    let ordering = SlideOrdering::resolve(&stream, &mut budget, &mut document.warnings);

    // A deck deep or wide enough to exhaust the walk budget is rejected
    // rather than reported as a short document.
    budget.check()?;

    let slide_offsets = ordering.slide_offsets(&stream, &mut budget);
    budget.check()?;
    if slide_offsets.is_empty() {
        return Err(DocumentError::Malformed {
            part: Some("PowerPoint Document".to_owned()),
            detail: "no slide records found".to_owned(),
        });
    }

    let masters = ordering.collect_masters(&stream, &mut budget);
    budget.check()?;

    let mut pictures = PictureStore {
        stream: read_stream(&mut compound, "/Pictures", options)?.unwrap_or_default(),
        offsets: collect_blip_store(&stream, ordering.document_offset, &mut budget),
        ..PictureStore::default()
    };
    budget.check()?;

    // slideId → presentation index, so sparsely-attached notes land on the
    // slide they belong to rather than the one at the same ordinal.
    let mut index_of_slide_id: HashMap<u32, usize> = HashMap::new();
    for (index, entry) in ordering.slides.iter().enumerate() {
        index_of_slide_id.insert(entry.id, index);
    }

    for (index, offset) in slide_offsets.iter().enumerate() {
        let master = masters.select(&stream, *offset, &mut budget);
        let shapes = collect_shapes(&stream, *offset, &mut budget);
        let slide = build_slide(
            shapes,
            master,
            &mut pictures,
            &mut document.assets,
            &mut document.warnings,
            options,
        );
        let mut unit = DocumentUnit::new(UnitKind::Slide, index, slide.title);
        unit.blocks = slide.blocks;
        if unit.label.is_none() {
            unit.label = Some(format!("Slide {}", index + 1));
        }
        document.units.push(unit);
    }

    if options.include_notes {
        for (slide_id, offset) in notes_records(&stream, &mut budget) {
            // A notes page's own text is styled by the notes master, which
            // the deck's master list does not carry; its runs still resolve,
            // just without per-level defaults.
            let shapes = collect_shapes(&stream, offset, &mut budget);
            let blocks = build_notes(shapes);
            if blocks.is_empty() {
                continue;
            }
            let index = index_of_slide_id
                .get(&slide_id)
                .copied()
                .unwrap_or(document.notes.len());
            document.notes.push(Note {
                id: format!("slide-{}-notes", index + 1),
                kind: NoteKind::SpeakerNote,
                blocks,
            });
        }
    }
    budget.check()?;

    document.warnings.push(ParseWarning {
        code: WarningCode::UnsupportedFeature,
        part: None,
        message: "legacy .ppt is recovered as slide text, outline lists inherited from the \
                  slide master, bold/italic runs, embedded bitmap pictures and speaker \
                  notes; a table's cells are recovered as separate paragraphs rather than \
                  as a table, and charts, metafile pictures and shape geometry are not \
                  retained"
            .to_owned(),
    });
    Ok(document)
}

fn read_stream(
    compound: &mut cfb::CompoundFile<Cursor<&[u8]>>,
    name: &str,
    options: &ParseOptions,
) -> Result<Option<Vec<u8>>, DocumentError> {
    if !compound.exists(name) {
        return Ok(None);
    }
    let mut stream = compound
        .open_stream(name)
        .map_err(|error| DocumentError::malformed(format!("cannot open {name}: {error}")))?;
    let declared = stream.len();
    if declared > options.limits.max_entry_bytes {
        return Err(DocumentError::ResourceLimit {
            limit: "max_entry_bytes",
            detail: format!("OLE stream {name} declares {declared} bytes"),
        });
    }
    let mut buffer = Vec::with_capacity(declared as usize);
    stream.read_to_end(&mut buffer)?;
    Ok(Some(buffer))
}

/// Bounds record-tree walking so a hostile or corrupt deck cannot spin.
struct Budget {
    records: usize,
    max_records: usize,
    depth: usize,
    max_depth: usize,
    /// Which budget ran out, if any.
    exhausted: Option<&'static str>,
}

impl Budget {
    fn new(options: &ParseOptions) -> Self {
        Self {
            records: 0,
            max_records: options.limits.max_xml_nodes,
            depth: 0,
            max_depth: options.limits.max_record_depth,
            exhausted: None,
        }
    }

    fn record(&mut self) -> bool {
        self.records += 1;
        self.records <= self.max_records
    }

    fn check(&self) -> Result<(), DocumentError> {
        match self.exhausted {
            Some(limit) => Err(DocumentError::ResourceLimit {
                limit,
                detail: "PowerPoint record tree exceeds the configured budget".to_owned(),
            }),
            None => Ok(()),
        }
    }
}

struct Record {
    version: u16,
    /// `recInstance`: which of several same-typed records this is. For
    /// `SlideListWithText` it selects slides/masters/notes, and for
    /// `TxMasterStyleAtom` it is the text type the styles apply to.
    instance: u16,
    kind: u16,
    body: std::ops::Range<usize>,
    /// Offset of the next record's header.
    next: usize,
}

fn read_record(stream: &[u8], at: usize) -> Option<Record> {
    let header = stream.get(at..at + 8)?;
    let ver_instance = u16::from_le_bytes([header[0], header[1]]);
    let kind = u16::from_le_bytes([header[2], header[3]]);
    let length = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
    let body_start = at + 8;
    let body_end = body_start.checked_add(length)?.min(stream.len());
    Some(Record {
        version: ver_instance & 0x000F,
        instance: ver_instance >> 4,
        kind,
        body: body_start..body_end,
        next: body_end,
    })
}

/// One entry of a `SlideListWithText`: where the page lives and what its id
/// is. Slides and masters use the same record shape.
#[derive(Clone, Copy)]
struct PersistEntry {
    persist_ref: u32,
    /// `slideId` for a slide, `masterId` for a master.
    id: u32,
}

/// Presentation order and slide identity, taken from the document container.
#[derive(Default)]
struct SlideOrdering {
    /// Slide entries in presentation order (`SlideListWithText` instance 0).
    slides: Vec<PersistEntry>,
    /// Master entries in master-list order (instance 1).
    masters: Vec<PersistEntry>,
    /// `persistId` → byte offset, from the persist directory.
    offsets: HashMap<u32, u32>,
    /// Where the document container that supplied the slide list starts. An
    /// edited deck keeps its superseded generations in the same stream, and
    /// the blip store has to be read from *this* one — taking the first store
    /// in stream order can pick up a stale one, which shifts every `pib`
    /// index and hands slides the wrong pictures.
    document_offset: Option<usize>,
}

impl SlideOrdering {
    fn resolve(stream: &[u8], budget: &mut Budget, warnings: &mut Vec<ParseWarning>) -> Self {
        let mut ordering = Self::default();
        // Which list the walker is currently inside. `SlideListWithText`
        // containers are siblings and never nest, so the instance of the last
        // one entered holds until the next one begins.
        let mut list_instance = LIST_INSTANCE_SLIDES;
        let mut current_document = None;
        walk(stream, 0, stream.len(), budget, &mut |record, stream| {
            match record.kind {
                RT_PERSIST_DIRECTORY_ATOM => {
                    ordering.read_persist_directory(&stream[record.body.clone()])
                }
                RT_DOCUMENT => current_document = Some(record.body.start - 8),
                RT_SLIDE_LIST_WITH_TEXT => list_instance = record.instance,
                RT_SLIDE_PERSIST_ATOM => {
                    let body = &stream[record.body.clone()];
                    if body.len() >= 16 {
                        let entry = PersistEntry {
                            persist_ref: read_u32(body, 0),
                            id: read_u32(body, 12),
                        };
                        match list_instance {
                            LIST_INSTANCE_SLIDES => {
                                ordering.document_offset = current_document;
                                ordering.slides.push(entry)
                            }
                            LIST_INSTANCE_MASTERS => ordering.masters.push(entry),
                            // The notes list is walked separately, by
                            // following each notes container's own
                            // `NotesAtom`.
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            // Only the document container holds the slide list; everything
            // else is walked purely to reach it.
            record.kind == RT_DOCUMENT
                || record.kind == RT_SLIDE_LIST_WITH_TEXT
                || record.version == CONTAINER_VERSION
        });
        if ordering.slides.is_empty() {
            warnings.push(ParseWarning {
                code: WarningCode::UnsupportedFeature,
                part: Some("PowerPoint Document".to_owned()),
                message: "deck declares no slide list; falling back to stream order".to_owned(),
            });
        }
        ordering
    }

    /// `PersistDirectoryAtom`: runs of `(persistId, count)` followed by that
    /// many stream offsets (MS-PPT §2.3.4).
    fn read_persist_directory(&mut self, body: &[u8]) {
        let mut at = 0usize;
        while at + 4 <= body.len() {
            let entry = read_u32(body, at);
            let first_id = entry & 0x000F_FFFF;
            let count = (entry >> 20) as usize;
            at += 4;
            for index in 0..count {
                if at + 4 > body.len() {
                    return;
                }
                self.offsets
                    .insert(first_id + index as u32, read_u32(body, at));
                at += 4;
            }
        }
    }

    /// Byte offsets of the slide containers, in presentation order.
    fn slide_offsets(&self, stream: &[u8], budget: &mut Budget) -> Vec<usize> {
        let resolved: Vec<usize> = self
            .slides
            .iter()
            .filter_map(|entry| self.offsets.get(&entry.persist_ref).copied())
            .map(|offset| offset as usize)
            // A persist entry can point at any record; only take the ones that
            // really are slides, so a stale directory cannot make us parse a
            // master as a slide.
            .filter(|offset| {
                read_record(stream, *offset).is_some_and(|record| record.kind == RT_SLIDE)
            })
            .collect();
        if !resolved.is_empty() {
            return resolved;
        }
        // No usable directory: stream order is the best remaining guess.
        let mut offsets = Vec::new();
        walk(stream, 0, stream.len(), budget, &mut |record, _| {
            if record.kind == RT_SLIDE {
                offsets.push(record.body.start - 8);
                return false;
            }
            record.version == CONTAINER_VERSION
        });
        offsets
    }

    /// The deck's masters, in master-list order.
    fn collect_masters(&self, stream: &[u8], budget: &mut Budget) -> Masters {
        let mut masters: Vec<(u32, MasterStyles)> = self
            .masters
            .iter()
            .filter_map(|entry| {
                let offset = *self.offsets.get(&entry.persist_ref)? as usize;
                let record = read_record(stream, offset)?;
                (record.kind == RT_MAIN_MASTER)
                    .then(|| (entry.id, master_styles(stream, offset, budget)))
            })
            .collect();
        if masters.is_empty() {
            // A deck with no master list still has masters in the stream, and
            // a single-master deck is the common case: taking them in stream
            // order gives every slide the same defaults it would have had.
            let mut offsets = Vec::new();
            walk(stream, 0, stream.len(), budget, &mut |record, _| {
                if record.kind == RT_MAIN_MASTER {
                    offsets.push(record.body.start - 8);
                    return false;
                }
                record.version == CONTAINER_VERSION
            });
            masters = offsets
                .into_iter()
                .map(|offset| (0, master_styles(stream, offset, budget)))
                .collect();
        }
        Masters { masters }
    }
}

/// One master's `TxMasterStyleAtom`s, keyed by the text type they style.
type MasterStyles = HashMap<u16, MasterLevels>;

/// Every master in the deck, plus the lookup a slide uses to pick its own.
struct Masters {
    masters: Vec<(u32, MasterStyles)>,
}

impl Masters {
    /// The styles for the master a slide references through
    /// `SlideAtom.masterIdRef`. The first master is the deterministic
    /// fallback, which is also correct for the overwhelmingly common
    /// single-master deck.
    fn select(&self, stream: &[u8], slide_offset: usize, budget: &mut Budget) -> &MasterStyles {
        static EMPTY: std::sync::LazyLock<MasterStyles> =
            std::sync::LazyLock::new(MasterStyles::new);
        let referenced = slide_master_id(stream, slide_offset, budget)
            .and_then(|id| self.masters.iter().find(|(master_id, _)| *master_id == id));
        match referenced.or_else(|| self.masters.first()) {
            Some((_, styles)) => styles,
            None => &EMPTY,
        }
    }
}

/// `SlideAtom.masterIdRef` (MS-PPT §2.5.10): the fourth 32-bit field, after
/// the 12-byte `SlideAtomLayout`.
fn slide_master_id(stream: &[u8], slide_offset: usize, budget: &mut Budget) -> Option<u32> {
    let container = read_record(stream, slide_offset)?;
    let mut master_id = None;
    walk(
        stream,
        container.body.start,
        container.body.end,
        budget,
        &mut |record, stream| {
            if record.kind == RT_SLIDE_ATOM {
                let body = &stream[record.body.clone()];
                if body.len() >= 16 {
                    master_id = Some(read_u32(body, 12));
                }
            }
            // The atom is a direct child of the slide container.
            false
        },
    );
    master_id
}

/// One master container's per-text-type level defaults.
fn master_styles(stream: &[u8], offset: usize, budget: &mut Budget) -> MasterStyles {
    let mut styles = MasterStyles::new();
    let Some(container) = read_record(stream, offset) else {
        return styles;
    };
    walk(
        stream,
        container.body.start,
        container.body.end,
        budget,
        &mut |record, stream| {
            if record.kind == RT_TX_MASTER_STYLE_ATOM {
                styles.entry(record.instance).or_insert_with(|| {
                    styletext::parse_master_style(&stream[record.body.clone()], record.instance)
                });
            }
            record.version == CONTAINER_VERSION
        },
    );
    styles
}

/// `(slideIdRef, offset)` for every notes container in the stream.
fn notes_records(stream: &[u8], budget: &mut Budget) -> Vec<(u32, usize)> {
    let mut found = Vec::new();
    let mut pending: Option<usize> = None;
    walk(stream, 0, stream.len(), budget, &mut |record, stream| {
        match record.kind {
            RT_NOTES => {
                pending = Some(record.body.start - 8);
                return true;
            }
            RT_NOTES_ATOM => {
                // NotesAtom starts with the id of the slide it annotates.
                let body = &stream[record.body.clone()];
                if body.len() >= 4
                    && let Some(offset) = pending.take()
                {
                    let slide_id = read_u32(body, 0);
                    // Ids with the high bit set are in the master range: this
                    // is the notes *master*, whose placeholder prompts are
                    // template chrome rather than anyone's speaker notes.
                    if slide_id & 0x8000_0000 == 0 {
                        found.push((slide_id, offset));
                    }
                }
            }
            _ => {}
        }
        record.version == CONTAINER_VERSION
    });
    found
}

/// Walk records between `at` and `end`, calling `visit`; descend into a
/// record only when `visit` returns `true`.
fn walk(
    stream: &[u8],
    mut at: usize,
    end: usize,
    budget: &mut Budget,
    visit: &mut dyn FnMut(&Record, &[u8]) -> bool,
) {
    // Exceeding a budget is recorded rather than returned, because `walk` is
    // driven by closures that cannot propagate an error; the caller checks
    // `Budget::exhausted` and turns it into a `ResourceLimit`. Silently
    // truncating instead would report a deliberately over-nested deck as a
    // successful — but incomplete — parse.
    if budget.depth >= budget.max_depth {
        budget.exhausted = Some("max_record_depth");
        return;
    }
    budget.depth += 1;
    while at + 8 <= end {
        if !budget.record() {
            budget.exhausted = Some("max_xml_nodes");
            break;
        }
        let Some(record) = read_record(stream, at) else {
            break;
        };
        // A zero-length record would leave the cursor stationary.
        if record.next <= at {
            break;
        }
        let descend = visit(&record, stream);
        if descend && record.version == CONTAINER_VERSION {
            walk(stream, record.body.start, record.body.end, budget, visit);
        }
        at = record.next;
    }
    budget.depth -= 1;
}

// ---------------------------------------------------------------------------
// Text shapes
// ---------------------------------------------------------------------------

/// Text placeholder kinds from `TextHeaderAtom` (MS-PPT §2.9.71).
const TEXT_TYPE_TITLE: u32 = 0;
const TEXT_TYPE_BODY: u32 = 1;
const TEXT_TYPE_CENTER_TITLE: u32 = 6;

/// One text shape: its placeholder type, its text, and the styling that
/// applies to that text.
#[derive(Default)]
struct Shape {
    text_type: u32,
    text: String,
    styles: Option<StyleRuns>,
}

/// What a slide container yields, in the order it appears: text shapes and
/// the pictures between them.
enum SlideItem {
    Text(Shape),
    /// A shape's `pib`: a 1-based index into the deck's blip store.
    Picture(u32),
}

/// Collect the text shapes of one slide or notes container, in the order they
/// appear.
///
/// A shape is opened by its `TextHeaderAtom` and closed by the next one, so
/// the text and style atoms in between belong to it — including
/// `StyleTextPropAtom`, which carries no identity of its own and is only
/// interpretable relative to the text it follows.
fn collect_shapes(stream: &[u8], offset: usize, budget: &mut Budget) -> Vec<SlideItem> {
    let Some(container) = read_record(stream, offset) else {
        return Vec::new();
    };
    let mut shapes: Vec<SlideItem> = Vec::new();
    let mut pending: Option<Shape> = None;
    walk(
        stream,
        container.body.start,
        container.body.end,
        budget,
        &mut |record, stream| {
            let body = &stream[record.body.clone()];
            match record.kind {
                OFFICE_ART_FOPT => {
                    // A shape's property table precedes its text, so a
                    // picture found here belongs before whatever text the
                    // same drawing emits next.
                    if let Some(index) = pictures::fopt_picture_index(body, record.instance) {
                        if let Some(shape) = pending.take() {
                            shapes.push(SlideItem::Text(shape));
                        }
                        shapes.push(SlideItem::Picture(index));
                    }
                }
                RT_TEXT_HEADER_ATOM => {
                    if let Some(shape) = pending.take() {
                        shapes.push(SlideItem::Text(shape));
                    }
                    pending = Some(Shape {
                        text_type: if body.len() >= 4 {
                            read_u32(body, 0)
                        } else {
                            TEXT_TYPE_BODY
                        },
                        ..Shape::default()
                    });
                }
                RT_TEXT_CHARS_ATOM => {
                    let text: String = char::decode_utf16(
                        body.chunks_exact(2)
                            .map(|pair| u16::from_le_bytes([pair[0], pair[1]])),
                    )
                    .map(|value| value.unwrap_or(char::REPLACEMENT_CHARACTER))
                    .collect();
                    push_text(&mut pending, text);
                }
                RT_TEXT_BYTES_ATOM => {
                    // Each byte is the low half of a UTF-16 code unit whose
                    // high half is zero, so this is Latin-1 by construction.
                    let text: String = body.iter().map(|byte| *byte as char).collect();
                    push_text(&mut pending, text);
                }
                RT_STYLE_TEXT_PROP_ATOM => {
                    if let Some(shape) = pending.as_mut() {
                        // Run lengths are counted in UTF-16 code units, which
                        // is what the text atoms store — not in `char`s.
                        let units = shape.text.chars().map(char::len_utf16).sum();
                        shape.styles = Some(styletext::parse_style_text(body, units));
                    }
                }
                _ => {}
            }
            record.version == CONTAINER_VERSION
        },
    );
    if let Some(shape) = pending.take() {
        shapes.push(SlideItem::Text(shape));
    }
    shapes.retain(|item| match item {
        SlideItem::Text(shape) => !shape.text.is_empty(),
        SlideItem::Picture(_) => true,
    });
    shapes
}

/// Text can precede any `TextHeaderAtom` in a malformed or unusual container;
/// treating it as body text keeps it rather than dropping it.
fn push_text(pending: &mut Option<Shape>, text: String) {
    match pending {
        Some(shape) => shape.text.push_str(&text),
        None => {
            *pending = Some(Shape {
                text_type: TEXT_TYPE_BODY,
                text,
                styles: None,
            });
        }
    }
}

/// One paragraph of a shape, with the properties that decide how it renders.
struct StyledParagraph {
    content: Vec<Inline>,
    depth: u16,
    bullet: bool,
}

/// Split a shape's text into paragraphs, resolving each character's emphasis
/// and each paragraph's bullet against the master's per-level defaults.
fn shape_paragraphs(shape: &Shape, levels: &MasterLevels) -> Vec<StyledParagraph> {
    let empty = StyleRuns::default();
    let styles = shape.styles.as_ref().unwrap_or(&empty);
    let level_default = |depth: u16| -> MasterLevel {
        levels
            .get(depth as usize)
            .copied()
            .unwrap_or_else(MasterLevel::default)
    };

    // Cursors over the two run lists, both counted in UTF-16 code units. A
    // shape with no styling at all leaves both at `usize::MAX`, so every
    // character resolves against the master defaults alone.
    let mut paragraph_runs = styles.paragraphs.iter();
    let mut paragraph_run = paragraph_runs.next();
    let mut paragraph_left = paragraph_run.map_or(usize::MAX, |run| run.count);
    let mut character_runs = styles.characters.iter();
    let mut character_run = character_runs.next();
    let mut character_left = character_run.map_or(usize::MAX, |run| run.count);

    let mut paragraphs: Vec<StyledParagraph> = Vec::new();
    let mut content: Vec<Inline> = Vec::new();
    let mut run_text = String::new();
    let mut run_style = Style::default();
    // The properties in force at the last character consumed. A shape whose
    // text does not end in a paragraph mark still has a final paragraph, and
    // by then the run cursor may already have moved past the run that styled
    // it — reading the cursor at that point silently gives that paragraph
    // depth 0 and no bullet.
    let mut last_properties: Option<(u16, bool)> = None;

    for character in shape.text.chars() {
        let depth = paragraph_run.map_or(0, |run| run.depth);
        let defaults = level_default(depth);
        let bullet = paragraph_run
            .and_then(|run| run.bullet)
            .or(defaults.bullet)
            .unwrap_or(false);
        last_properties = Some((depth, bullet));
        let style = Style {
            bold: character_run
                .and_then(|run| run.bold)
                .or(defaults.bold)
                .unwrap_or(false),
            italic: character_run
                .and_then(|run| run.italic)
                .or(defaults.italic)
                .unwrap_or(false),
            ..Style::default()
        };

        match character {
            PARAGRAPH_MARK => {
                flush_run(&mut run_text, &run_style, &mut content);
                push_paragraph(&mut paragraphs, &mut content, depth, bullet);
            }
            LINE_BREAK => {
                flush_run(&mut run_text, &run_style, &mut content);
                content.push(Inline::LineBreak);
            }
            _ => {
                if style != run_style {
                    flush_run(&mut run_text, &run_style, &mut content);
                    run_style = style;
                }
                run_text.push(character);
            }
        }

        // Both run lists advance by the character's UTF-16 width, not by one
        // per `char`: an astral character consumes two units of each run.
        let width = character.len_utf16();
        character_left = character_left.saturating_sub(width);
        if character_left == 0 {
            character_run = character_runs.next();
            character_left = character_run.map_or(usize::MAX, |run| run.count);
        }
        paragraph_left = paragraph_left.saturating_sub(width);
        if paragraph_left == 0 {
            paragraph_run = paragraph_runs.next();
            paragraph_left = paragraph_run.map_or(usize::MAX, |run| run.count);
        }
    }

    flush_run(&mut run_text, &run_style, &mut content);
    if !content.is_empty() {
        let (depth, bullet) = last_properties.unwrap_or((0, false));
        push_paragraph(&mut paragraphs, &mut content, depth, bullet);
    }
    paragraphs
}

fn flush_run(run_text: &mut String, style: &Style, content: &mut Vec<Inline>) {
    if run_text.is_empty() {
        return;
    }
    content.push(Inline::Text {
        text: std::mem::take(run_text),
        style: style.clone(),
    });
}

fn push_paragraph(
    paragraphs: &mut Vec<StyledParagraph>,
    content: &mut Vec<Inline>,
    depth: u16,
    bullet: bool,
) {
    let content = trim_paragraph(std::mem::take(content));
    if content.is_empty() {
        return;
    }
    paragraphs.push(StyledParagraph {
        content,
        depth: depth.min(MAX_LIST_DEPTH),
        bullet,
    });
}

/// Trim the paragraph's outer whitespace without disturbing the run
/// boundaries inside it, and drop runs that were only whitespace.
fn trim_paragraph(content: Vec<Inline>) -> Vec<Inline> {
    let mut content = content;
    while let Some(first) = content.first_mut() {
        if let Inline::Text { text, .. } = first {
            let trimmed = text.trim_start();
            if trimmed.len() != text.len() {
                *text = trimmed.to_owned();
            }
            if text.is_empty() {
                content.remove(0);
                continue;
            }
        }
        break;
    }
    while let Some(last) = content.last_mut() {
        if let Inline::Text { text, .. } = last {
            let trimmed = text.trim_end();
            if trimmed.len() != text.len() {
                *text = trimmed.to_owned();
            }
            if text.is_empty() {
                content.pop();
                continue;
            }
        }
        break;
    }
    content
}

/// The deck's blip store: where each `pib` index's picture lives, and the
/// stream it lives in.
#[derive(Default)]
struct PictureStore {
    /// The `Pictures` OLE stream, empty when the deck has none.
    stream: Vec<u8>,
    /// `foDelay` per blip-store entry, in store order; `pib` is a 1-based
    /// index into this.
    offsets: Vec<u32>,
    /// `pib` → the asset it registered, so a picture used on several slides
    /// is stored once. `None` records a picture that could not be decoded,
    /// which also keeps its warning from repeating per use.
    resolved: HashMap<u32, Option<AssetId>>,
}

impl PictureStore {
    fn resolve(
        &mut self,
        index: u32,
        assets: &mut Vec<Asset>,
        warnings: &mut Vec<ParseWarning>,
        options: &ParseOptions,
    ) -> Option<AssetId> {
        if let Some(cached) = self.resolved.get(&index) {
            return cached.clone();
        }
        let resolved = self.decode(index, assets, warnings, options);
        self.resolved.insert(index, resolved.clone());
        resolved
    }

    fn decode(
        &self,
        index: u32,
        assets: &mut Vec<Asset>,
        warnings: &mut Vec<ParseWarning>,
        options: &ParseOptions,
    ) -> Option<AssetId> {
        // `pib` is 1-based; a 0 or out-of-range index is a dangling
        // reference rather than a picture.
        let offset = index
            .checked_sub(1)
            .and_then(|slot| self.offsets.get(slot as usize).copied())?;
        let Some(picture) = pictures::picture_at(&self.stream, offset as usize) else {
            let kind = pictures::record_kind_at(&self.stream, offset as usize);
            warnings.push(ParseWarning {
                code: WarningCode::AssetDropped,
                part: Some("Pictures".to_owned()),
                message: match kind {
                    Some(kind) if pictures::is_unsupported_blip(kind) => format!(
                        "picture {index} is a metafile blip (record 0x{kind:04X}), \
                         which is not decoded"
                    ),
                    Some(kind) => {
                        format!("picture {index} is an unrecognised blip (record 0x{kind:04X})")
                    }
                    None => format!("picture {index} points outside the Pictures stream"),
                },
            });
            return None;
        };
        if picture.bytes.len() > options.limits.max_asset_bytes {
            warnings.push(ParseWarning {
                code: WarningCode::AssetDropped,
                part: Some("Pictures".to_owned()),
                message: format!("picture exceeds {} bytes", options.limits.max_asset_bytes),
            });
            return None;
        }
        let sha256 = format!("{:x}", Sha256::digest(picture.bytes));
        let id = format!("asset-{}", &sha256[..16]);
        if !assets.iter().any(|asset| asset.id == id) {
            assets.push(Asset {
                id: id.clone(),
                media_type: picture.media_type.to_owned(),
                filename: None,
                byte_length: picture.bytes.len(),
                sha256,
                path: None,
                bytes: options.include_assets.then(|| picture.bytes.to_vec()),
            });
        }
        Some(id)
    }
}

/// The blip store entries of the document container at `document_offset`, in
/// store order — `pib` is a 1-based index into exactly this sequence.
fn collect_blip_store(
    stream: &[u8],
    document_offset: Option<usize>,
    budget: &mut Budget,
) -> Vec<u32> {
    // Scoping to the current document container is what keeps a superseded
    // generation's store out of the index; without a document to scope to,
    // scanning the whole stream is still better than having no pictures.
    let (start, end) = match document_offset.and_then(|offset| read_record(stream, offset)) {
        Some(document) => (document.body.start, document.body.end),
        None => (0, stream.len()),
    };
    let mut offsets = Vec::new();
    let mut inside_store = false;
    walk(stream, start, end, budget, &mut |record, stream| {
        match record.kind {
            OFFICE_ART_BSTORE => {
                // Only the first store is the deck's; a later one would
                // continue the same index space and mis-number every entry.
                if inside_store {
                    return false;
                }
                inside_store = true;
                return true;
            }
            OFFICE_ART_FBSE if inside_store => {
                offsets
                    .push(pictures::fbse_picture_offset(&stream[record.body.clone()]).unwrap_or(0));
            }
            _ => {}
        }
        record.version == CONTAINER_VERSION
    });
    offsets
}

/// A slide's recovered content.
struct Slide {
    title: Option<String>,
    blocks: Vec<Block>,
}

fn build_slide(
    items: Vec<SlideItem>,
    master: &MasterStyles,
    store: &mut PictureStore,
    assets: &mut Vec<Asset>,
    warnings: &mut Vec<ParseWarning>,
    options: &ParseOptions,
) -> Slide {
    let mut title = None;
    let mut blocks = Vec::new();
    // Consecutive bulleted paragraphs form one list; anything else ends it.
    let mut list_run: Vec<(u16, Vec<Block>)> = Vec::new();

    for item in items {
        let shape = match item {
            SlideItem::Text(shape) => shape,
            SlideItem::Picture(index) => {
                flush_list(&mut blocks, &mut list_run);
                if let Some(asset_id) = store.resolve(index, assets, warnings, options) {
                    blocks.push(Block::Figure {
                        asset_id: Some(asset_id),
                        alt: None,
                        caption: Vec::new(),
                    });
                }
                continue;
            }
        };
        let levels = master
            .get(&(shape.text_type as u16))
            .cloned()
            .unwrap_or_default();
        let mut paragraphs = shape_paragraphs(&shape, &levels).into_iter();

        // The first paragraph of the first title placeholder names the slide;
        // it becomes both the unit's label and its leading heading.
        if matches!(shape.text_type, TEXT_TYPE_TITLE | TEXT_TYPE_CENTER_TITLE)
            && title.is_none()
            && let Some(first) = paragraphs.next()
        {
            flush_list(&mut blocks, &mut list_run);
            title = Some(plain_text(&first.content));
            blocks.push(Block::Heading {
                level: 1,
                content: first.content,
            });
        }

        for paragraph in paragraphs {
            if paragraph.bullet {
                list_run.push((
                    paragraph.depth,
                    vec![Block::Paragraph {
                        content: paragraph.content,
                    }],
                ));
            } else {
                flush_list(&mut blocks, &mut list_run);
                blocks.push(Block::Paragraph {
                    content: paragraph.content,
                });
            }
        }
    }
    flush_list(&mut blocks, &mut list_run);
    Slide { title, blocks }
}

/// A notes page carries its own copies of the slide's title and body
/// placeholders, so those are excluded — everything else on the page is the
/// speaker's note.
///
/// Keying on the notes placeholder type alone is not enough: a notes body
/// written by a producer that does not mark placeholders (LibreOffice's PPT
/// export marks every shape "other") would then be dropped entirely, which is
/// how a deck ends up silently noteless. The repeated title/body placeholders
/// are normally empty anyway, and `collect_shapes` already discards empty
/// shapes.
fn build_notes(items: Vec<SlideItem>) -> Vec<Block> {
    let levels = MasterLevels::new();
    let mut blocks = Vec::new();
    let mut list_run: Vec<(u16, Vec<Block>)> = Vec::new();
    for shape in items
        .iter()
        .filter_map(|item| match item {
            SlideItem::Text(shape) => Some(shape),
            // A notes page's picture is the thumbnail of its own slide.
            SlideItem::Picture(_) => None,
        })
        .filter(|shape| !matches!(shape.text_type, TEXT_TYPE_TITLE | TEXT_TYPE_CENTER_TITLE))
    {
        for paragraph in shape_paragraphs(shape, &levels) {
            if paragraph.bullet {
                list_run.push((
                    paragraph.depth,
                    vec![Block::Paragraph {
                        content: paragraph.content,
                    }],
                ));
            } else {
                flush_list(&mut blocks, &mut list_run);
                blocks.push(Block::Paragraph {
                    content: paragraph.content,
                });
            }
        }
    }
    flush_list(&mut blocks, &mut list_run);
    blocks
}

fn plain_text(content: &[Inline]) -> String {
    let mut out = String::new();
    for inline in content {
        match inline {
            Inline::Text { text, .. } => out.push_str(text),
            Inline::LineBreak => out.push(' '),
            _ => {}
        }
    }
    out.trim().to_owned()
}

fn flush_list(blocks: &mut Vec<Block>, run: &mut Vec<(u16, Vec<Block>)>) {
    if run.is_empty() {
        return;
    }
    let mut entries = std::mem::take(run);
    let mut cursor = 0usize;
    let base = entries.iter().map(|(depth, _)| *depth).min().unwrap_or(0);
    if let Some(list) = build_list(&mut entries, &mut cursor, base) {
        blocks.push(list);
    }
}

/// Turn a flat run of `(depth, blocks)` entries into nested lists: an entry
/// deeper than the one before it becomes a child list of that entry rather
/// than a sibling.
fn build_list(
    entries: &mut Vec<(u16, Vec<Block>)>,
    cursor: &mut usize,
    base: u16,
) -> Option<Block> {
    let mut items: Vec<ListItem> = Vec::new();
    while *cursor < entries.len() {
        let depth = entries[*cursor].0;
        if depth < base {
            break;
        }
        if depth == base {
            let blocks = std::mem::take(&mut entries[*cursor].1);
            items.push(ListItem { blocks });
            *cursor += 1;
        } else if let Some(nested) = build_list(entries, cursor, depth) {
            // A deeper entry with no shallower entry before it (a shape whose
            // outline starts indented) still has to land somewhere.
            match items.last_mut() {
                Some(item) => item.blocks.push(nested),
                None => items.push(ListItem {
                    blocks: vec![nested],
                }),
            }
        }
    }
    if items.is_empty() {
        return None;
    }
    Some(Block::List {
        list: List {
            marker: ListMarker::Bullet,
            start: None,
            items,
        },
    })
}

fn read_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

#[cfg(test)]
mod tests {
    use super::*;
    use styletext::{CharacterRun, ParagraphRun};

    fn record(kind: u16, version: u16, instance: u16, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&((instance << 4) | version).to_le_bytes());
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(body);
        out
    }

    fn container(kind: u16, instance: u16, body: &[u8]) -> Vec<u8> {
        record(kind, CONTAINER_VERSION, instance, body)
    }

    /// Wrap streams into an OLE2 compound file, so a synthetic deck can be
    /// driven through the real `parse` entry point rather than through its
    /// internals.
    fn compound(streams: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write;
        let mut file =
            cfb::CompoundFile::create(Cursor::new(Vec::new())).expect("a new compound file");
        for (name, bytes) in streams {
            let mut stream = file.create_stream(name).expect("a new stream");
            stream.write_all(bytes).expect("writing the stream");
        }
        file.into_inner().into_inner()
    }

    fn shape(text: &str, text_type: u32, styles: Option<StyleRuns>) -> Shape {
        Shape {
            text_type,
            text: text.to_owned(),
            styles,
        }
    }

    fn paragraph_run(count: usize, depth: u16, bullet: Option<bool>) -> ParagraphRun {
        ParagraphRun {
            count,
            depth,
            bullet,
        }
    }

    /// `build_slide` with an empty picture store, for the text-only cases.
    fn slide_of(items: Vec<SlideItem>, master: &MasterStyles) -> Slide {
        let mut store = PictureStore::default();
        let mut assets = Vec::new();
        let mut warnings = Vec::new();
        build_slide(
            items,
            master,
            &mut store,
            &mut assets,
            &mut warnings,
            &ParseOptions::default(),
        )
    }

    fn text_of(block: &Block) -> String {
        match block {
            Block::Paragraph { content } | Block::Heading { content, .. } => plain_text(content),
            _ => String::new(),
        }
    }

    #[test]
    fn text_atoms_split_on_paragraph_marks() {
        let paragraphs = shape_paragraphs(&shape("First\rSecond", 1, None), &MasterLevels::new());
        let texts: Vec<String> = paragraphs.iter().map(|p| plain_text(&p.content)).collect();
        assert_eq!(texts, vec!["First", "Second"]);
    }

    #[test]
    fn a_title_placeholder_becomes_the_slide_heading() {
        let slide = slide_of(
            vec![
                SlideItem::Text(shape("Quarterly Review", TEXT_TYPE_TITLE, None)),
                SlideItem::Text(shape("Revenue grew", TEXT_TYPE_BODY, None)),
            ],
            &MasterStyles::new(),
        );
        assert_eq!(slide.title.as_deref(), Some("Quarterly Review"));
        assert!(matches!(slide.blocks[0], Block::Heading { level: 1, .. }));
        assert_eq!(text_of(&slide.blocks[1]), "Revenue grew");
    }

    #[test]
    fn a_character_run_turns_into_a_bold_inline() {
        let styles = StyleRuns {
            paragraphs: vec![paragraph_run(21, 0, None)],
            characters: vec![
                CharacterRun {
                    count: 12,
                    bold: Some(false),
                    italic: None,
                },
                CharacterRun {
                    count: 8,
                    bold: Some(true),
                    italic: None,
                },
            ],
        };
        let paragraphs = shape_paragraphs(
            &shape("Plain text, bold one", 1, Some(styles)),
            &MasterLevels::new(),
        );
        assert_eq!(paragraphs.len(), 1);
        let bold: Vec<&str> = paragraphs[0]
            .content
            .iter()
            .filter_map(|inline| match inline {
                Inline::Text { text, style } if style.bold => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(bold, vec!["bold one"]);
    }

    #[test]
    fn a_shape_with_no_styling_inherits_the_masters_bullet_and_emphasis() {
        // The whole point of master inheritance: this shape carries no
        // StyleTextPropAtom at all, so without the master's defaults it would
        // render as an unbulleted, unemphasised paragraph.
        let levels = vec![MasterLevel {
            bullet: Some(true),
            bold: Some(true),
            italic: None,
        }];
        let paragraphs = shape_paragraphs(&shape("Template driven", 1, None), &levels);
        assert!(paragraphs[0].bullet);
        assert!(matches!(
            &paragraphs[0].content[0],
            Inline::Text { style, .. } if style.bold
        ));
    }

    #[test]
    fn a_local_run_overrides_the_masters_default() {
        let levels = vec![MasterLevel {
            bullet: Some(true),
            bold: Some(true),
            italic: None,
        }];
        let styles = StyleRuns {
            paragraphs: vec![paragraph_run(6, 0, Some(false))],
            characters: vec![CharacterRun {
                count: 6,
                bold: Some(false),
                italic: None,
            }],
        };
        let paragraphs = shape_paragraphs(&shape("Local", 1, Some(styles)), &levels);
        assert!(!paragraphs[0].bullet);
        assert!(matches!(
            &paragraphs[0].content[0],
            Inline::Text { style, .. } if !style.bold
        ));
    }

    #[test]
    fn outline_depth_becomes_a_nested_list() {
        let styles = StyleRuns {
            paragraphs: vec![
                paragraph_run(4, 0, Some(true)),
                paragraph_run(7, 1, Some(true)),
                paragraph_run(4, 0, Some(true)),
            ],
            characters: Vec::new(),
        };
        let slide = slide_of(
            vec![SlideItem::Text(shape(
                "Top\rNested\rEnd",
                TEXT_TYPE_BODY,
                Some(styles),
            ))],
            &MasterStyles::new(),
        );
        let Block::List { list } = &slide.blocks[0] else {
            panic!("expected a list, got {:?}", slide.blocks);
        };
        assert_eq!(list.items.len(), 2);
        assert_eq!(text_of(&list.items[0].blocks[0]), "Top");
        // The deeper paragraph is a child of the item above it, not a sibling.
        let Block::List { list: nested } = &list.items[0].blocks[1] else {
            panic!("expected a nested list");
        };
        assert_eq!(text_of(&nested.items[0].blocks[0]), "Nested");
        assert_eq!(text_of(&list.items[1].blocks[0]), "End");
    }

    #[test]
    fn an_absurd_outline_depth_cannot_recurse_without_bound() {
        // Depth is a u16 and list nesting is built recursively, so an atom
        // claiming a huge depth has to be clamped before it gets there.
        let styles = StyleRuns {
            paragraphs: vec![paragraph_run(4, u16::MAX, Some(true))],
            characters: Vec::new(),
        };
        let paragraphs = shape_paragraphs(&shape("Deep", 1, Some(styles)), &MasterLevels::new());
        assert_eq!(paragraphs[0].depth, MAX_LIST_DEPTH);
    }

    #[test]
    fn a_notes_page_drops_its_repeated_slide_title_but_keeps_the_note() {
        // A notes container carries its own copy of the slide's title
        // placeholder; taking every shape would duplicate the slide title
        // into its own note.
        let blocks = build_notes(vec![
            SlideItem::Text(shape("Deck Title Slide", TEXT_TYPE_TITLE, None)),
            // Text type 2 is the notes placeholder.
            SlideItem::Text(shape("Speaker note", 2, None)),
        ]);
        assert_eq!(blocks.len(), 1);
        assert_eq!(text_of(&blocks[0]), "Speaker note");
    }

    #[test]
    fn a_note_written_without_a_placeholder_type_is_still_a_note() {
        // LibreOffice's PPT export marks every text shape "other" (type 4),
        // so a reader that keeps only type-2 shapes silently produces a deck
        // with no speaker notes at all.
        let blocks = build_notes(vec![SlideItem::Text(shape("Speaker note", 4, None))]);
        assert_eq!(blocks.len(), 1);
        assert_eq!(text_of(&blocks[0]), "Speaker note");
    }

    #[test]
    fn a_zero_length_record_does_not_stall_the_walker() {
        // A record claiming zero length would leave the cursor where it was;
        // the walker must stop instead of looping forever.
        let stream = record(RT_SLIDE, 0, 0, &[]);
        let options = ParseOptions::default();
        let mut budget = Budget::new(&options);
        let mut seen = 0;
        walk(&stream, 0, stream.len(), &mut budget, &mut |_, _| {
            seen += 1;
            false
        });
        assert_eq!(seen, 1);
    }

    #[test]
    fn persist_directory_entries_map_ids_to_offsets() {
        let mut ordering = SlideOrdering::default();
        // One run: first id 5, two entries, offsets 100 and 200.
        let mut body = Vec::new();
        body.extend_from_slice(&(5u32 | (2u32 << 20)).to_le_bytes());
        body.extend_from_slice(&100u32.to_le_bytes());
        body.extend_from_slice(&200u32.to_le_bytes());
        ordering.read_persist_directory(&body);
        assert_eq!(ordering.offsets.get(&5), Some(&100));
        assert_eq!(ordering.offsets.get(&6), Some(&200));
    }

    #[test]
    fn a_truncated_persist_directory_stops_cleanly() {
        let mut ordering = SlideOrdering::default();
        // Declares three offsets but supplies one.
        let mut body = Vec::new();
        body.extend_from_slice(&(1u32 | (3u32 << 20)).to_le_bytes());
        body.extend_from_slice(&42u32.to_le_bytes());
        ordering.read_persist_directory(&body);
        assert_eq!(ordering.offsets.len(), 1);
    }

    #[test]
    fn master_and_notes_list_entries_stay_out_of_the_slide_order() {
        // All three SlideListWithText instances hold the same record type;
        // only the container's instance says which is which. Mixing them
        // shifts every slide id, which misattributes speaker notes.
        let persist_atom = |persist_ref: u32, id: u32| {
            let mut body = persist_ref.to_le_bytes().to_vec();
            body.extend_from_slice(&[0u8; 8]);
            body.extend_from_slice(&id.to_le_bytes());
            record(RT_SLIDE_PERSIST_ATOM, 0, 0, &body)
        };
        let masters = record(
            RT_SLIDE_LIST_WITH_TEXT,
            CONTAINER_VERSION,
            LIST_INSTANCE_MASTERS,
            &persist_atom(1, 0x8000_0001),
        );
        let slides = record(
            RT_SLIDE_LIST_WITH_TEXT,
            CONTAINER_VERSION,
            LIST_INSTANCE_SLIDES,
            &[persist_atom(2, 256), persist_atom(3, 257)].concat(),
        );
        let notes = record(
            RT_SLIDE_LIST_WITH_TEXT,
            CONTAINER_VERSION,
            2,
            &persist_atom(4, 0),
        );
        let document = record(
            RT_DOCUMENT,
            CONTAINER_VERSION,
            0,
            &[masters, slides, notes].concat(),
        );

        let options = ParseOptions::default();
        let mut budget = Budget::new(&options);
        let mut warnings = Vec::new();
        let ordering = SlideOrdering::resolve(&document, &mut budget, &mut warnings);
        assert_eq!(
            ordering.slides.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![256, 257]
        );
        assert_eq!(ordering.masters.len(), 1);
    }

    /// A deck with one slide holding a picture shape and a text shape, built
    /// end to end so the whole `pib` → blip store → `Pictures` stream chain
    /// is exercised through the real entry point.
    ///
    /// With `stale_store`, a second blip store — belonging to a superseded
    /// edit generation, outside the current document container — precedes it
    /// in the stream, pointing at a different picture.
    fn deck_with_a_picture(pib: u32, stale_store: bool) -> Vec<u8> {
        // The stale picture sits first in the Pictures stream, the real one
        // after it, so picking the wrong store yields the wrong bytes rather
        // than nothing.
        let blip = |payload: &[u8]| {
            let mut body = vec![0u8; 17];
            body.extend_from_slice(payload);
            record(0xF01E, 0, 0x6E0, &body)
        };
        let stale_picture = blip(b"STALEPNG");
        let real_picture_offset = stale_picture.len() as u32;
        let mut pictures = stale_picture;
        pictures.extend_from_slice(&blip(b"PNGBYTES"));

        let store = |picture_offset: u32| {
            let mut fbse = vec![0u8; 28];
            fbse.extend_from_slice(&picture_offset.to_le_bytes()); // foDelay
            fbse.extend_from_slice(&[0, 0, 0, 0]); // unused1, cbName, unused2, unused3
            container(
                0x040B,
                0,
                &container(
                    0xF000,
                    0,
                    &container(OFFICE_ART_BSTORE, 1, &record(OFFICE_ART_FBSE, 2, 0, &fbse)),
                ),
            )
        };
        let drawing_group = store(real_picture_offset);

        // The slide: a picture shape, then a text shape.
        let mut properties = PROPERTY_PIB_FOR_TEST.to_le_bytes().to_vec();
        properties.extend_from_slice(&pib.to_le_bytes());
        let picture_shape = container(0xF004, 0, &record(OFFICE_ART_FOPT, 3, 1, &properties));
        let text_shape = container(
            0xF004,
            0,
            &[
                record(RT_TEXT_HEADER_ATOM, 0, 0, &1u32.to_le_bytes()),
                record(RT_TEXT_BYTES_ATOM, 0, 0, b"After the picture"),
            ]
            .concat(),
        );
        let drawing = container(
            0x040C,
            0,
            &container(
                0xF002,
                0,
                &container(0xF003, 0, &[picture_shape, text_shape].concat()),
            ),
        );
        let slide_atom = record(RT_SLIDE_ATOM, 2, 0, &[0u8; 24]);
        let slide = container(RT_SLIDE, 0, &[slide_atom, drawing].concat());

        let mut persist_atom = 4u32.to_le_bytes().to_vec();
        persist_atom.extend_from_slice(&[0u8; 8]);
        persist_atom.extend_from_slice(&256u32.to_le_bytes());
        persist_atom.extend_from_slice(&[0u8; 4]);
        let slide_list = container(
            RT_SLIDE_LIST_WITH_TEXT,
            LIST_INSTANCE_SLIDES,
            &record(RT_SLIDE_PERSIST_ATOM, 0, 0, &persist_atom),
        );
        let document = container(RT_DOCUMENT, 0, &[drawing_group, slide_list].concat());

        let mut stream = Vec::new();
        if stale_store {
            stream.extend_from_slice(&store(0));
        }
        let document_offset = stream.len() as u32;
        stream.extend_from_slice(&document);
        let slide_offset = stream.len() as u32;
        stream.extend_from_slice(&slide);
        // Persist directory: ids 1..=4, of which only 4 (the slide) is used.
        let mut directory = (1u32 | (4u32 << 20)).to_le_bytes().to_vec();
        for offset in [document_offset, 0, 0, slide_offset] {
            directory.extend_from_slice(&offset.to_le_bytes());
        }
        stream.extend_from_slice(&record(RT_PERSIST_DIRECTORY_ATOM, 0, 0, &directory));

        compound(&[("/PowerPoint Document", &stream), ("/Pictures", &pictures)])
    }

    /// `pib`'s property id, duplicated here rather than exported from
    /// `pictures` so the test states the value it depends on.
    const PROPERTY_PIB_FOR_TEST: u16 = 0x0104;

    #[test]
    fn a_picture_shape_becomes_a_figure_with_the_bytes_it_points_at() {
        let deck = deck_with_a_picture(1, false);
        let document = parse(&deck, &ParseOptions::default()).expect("a parseable deck");
        let blocks = &document.units[0].blocks;
        let Block::Figure { asset_id, .. } = &blocks[0] else {
            panic!("expected a figure first, got {blocks:?}");
        };
        // The figure comes before the text of the shape that follows it.
        assert_eq!(text_of(&blocks[1]), "After the picture");
        let asset = document
            .assets
            .iter()
            .find(|asset| Some(&asset.id) == asset_id.as_ref())
            .expect("the figure's asset");
        assert_eq!(asset.media_type, "image/png");
        assert_eq!(asset.bytes.as_deref(), Some(b"PNGBYTES".as_slice()));
    }

    #[test]
    fn a_dangling_picture_reference_is_dropped_rather_than_mispointed() {
        // `pib` is 1-based, so index 2 is past the single-entry store here.
        // Treating an out-of-range index as an offset — or as 0-based — would
        // silently attach some other slide's picture.
        let deck = deck_with_a_picture(2, false);
        let document = parse(&deck, &ParseOptions::default()).expect("a parseable deck");
        assert!(document.assets.is_empty());
        assert!(
            !document.units[0]
                .blocks
                .iter()
                .any(|block| matches!(block, Block::Figure { .. }))
        );
        assert_eq!(text_of(&document.units[0].blocks[0]), "After the picture");
    }

    #[test]
    fn a_superseded_generations_blip_store_does_not_shift_the_picture_index() {
        // `pib` indexes the *current* document's store. A stream-order scan
        // finds the stale store first, and then index 1 resolves to whatever
        // picture that older edit had — a wrong image, silently, with no
        // error anywhere.
        let deck = deck_with_a_picture(1, true);
        let document = parse(&deck, &ParseOptions::default()).expect("a parseable deck");
        let asset = document.assets.first().expect("the figure's asset");
        assert_eq!(asset.bytes.as_deref(), Some(b"PNGBYTES".as_slice()));
    }
}
