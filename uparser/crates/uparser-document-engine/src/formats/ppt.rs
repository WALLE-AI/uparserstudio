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

use crate::{
    Block, CanonicalDocument, DocumentError, DocumentFormat, DocumentUnit, Inline, Note, NoteKind,
    ParseOptions, ParseWarning, UnitKind, WarningCode,
};
use std::collections::HashMap;
use std::io::{Cursor, Read};

// Record types (MS-PPT §2.13.24).
const RT_DOCUMENT: u16 = 0x03E8;
const RT_SLIDE: u16 = 0x03EE;
const RT_NOTES: u16 = 0x03F0;
const RT_NOTES_ATOM: u16 = 0x03F1;
const RT_SLIDE_PERSIST_ATOM: u16 = 0x03F3;
const RT_SLIDE_LIST_WITH_TEXT: u16 = 0x0FF0;
const RT_TEXT_HEADER_ATOM: u16 = 0x0F9F;
const RT_TEXT_CHARS_ATOM: u16 = 0x0FA0;
const RT_TEXT_BYTES_ATOM: u16 = 0x0FA8;
const RT_PERSIST_DIRECTORY_ATOM: u16 = 0x1772;

/// A record whose version nibble is 0xF contains other records rather than
/// data (MS-PPT §2.3.1).
const CONTAINER_VERSION: u16 = 0xF;

/// PowerPoint's own paragraph separator inside a text atom.
const PARAGRAPH_MARK: char = '\r';
const LINE_BREAK: char = '\u{b}';

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

    // slideId → presentation index, so sparsely-attached notes land on the
    // slide they belong to rather than the one at the same ordinal.
    let mut index_of_slide_id: HashMap<u32, usize> = HashMap::new();
    for (index, id) in ordering.slide_ids.iter().enumerate() {
        index_of_slide_id.insert(*id, index);
    }

    for (index, offset) in slide_offsets.iter().enumerate() {
        let mut text = SlideText::default();
        collect_text(&stream, *offset, &mut budget, &mut text)?;
        let mut unit = DocumentUnit::new(UnitKind::Slide, index, text.title.clone());
        unit.blocks = text.into_blocks();
        if unit.label.is_none() {
            unit.label = Some(format!("Slide {}", index + 1));
        }
        document.units.push(unit);
    }

    if options.include_notes {
        for (slide_id, offset) in notes_records(&stream, &mut budget) {
            let mut text = SlideText::default();
            collect_text(&stream, offset, &mut budget, &mut text)?;
            let blocks = text.into_note_blocks();
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

    document.warnings.push(ParseWarning {
        code: WarningCode::UnsupportedFeature,
        part: None,
        message: "legacy .ppt is recovered as slide text and speaker notes; \
                  master/layout inheritance, tables, charts and images are not retained"
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
    /// `recInstance`: read for completeness of the record header; the text
    /// placeholder type comes from `TextHeaderAtom`'s body instead.
    #[allow(dead_code)]
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

/// Presentation order and slide identity, taken from the document container.
#[derive(Default)]
struct SlideOrdering {
    /// `persistIdRef` per slide, in presentation order.
    persist_refs: Vec<u32>,
    /// `slideId` per slide, in the same order.
    slide_ids: Vec<u32>,
    /// `persistId` → byte offset, from the persist directory.
    offsets: HashMap<u32, u32>,
}

impl SlideOrdering {
    fn resolve(stream: &[u8], budget: &mut Budget, warnings: &mut Vec<ParseWarning>) -> Self {
        let mut ordering = Self::default();
        walk(stream, 0, stream.len(), budget, &mut |record, stream| {
            match record.kind {
                RT_PERSIST_DIRECTORY_ATOM => {
                    ordering.read_persist_directory(&stream[record.body.clone()])
                }
                RT_SLIDE_PERSIST_ATOM => {
                    let body = &stream[record.body.clone()];
                    if body.len() >= 16 {
                        ordering.persist_refs.push(read_u32(body, 0));
                        ordering.slide_ids.push(read_u32(body, 12));
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
        if ordering.persist_refs.is_empty() {
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
            .persist_refs
            .iter()
            .filter_map(|reference| self.offsets.get(reference).copied())
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
                    found.push((read_u32(body, 0), offset));
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

/// Text harvested from one slide or notes container.
#[derive(Default)]
struct SlideText {
    title: Option<String>,
    paragraphs: Vec<String>,
    /// Text explicitly typed as a notes placeholder. A notes container also
    /// repeats its slide's title and body, so the speaker's note has to be
    /// picked out by placeholder type rather than by taking everything.
    notes: Vec<String>,
}

impl SlideText {
    fn into_blocks(self) -> Vec<Block> {
        let mut blocks = Vec::new();
        if let Some(title) = self.title {
            blocks.push(Block::Heading {
                level: 1,
                content: vec![Inline::text(title)],
            });
        }
        blocks.extend(self.paragraphs.into_iter().map(Block::paragraph));
        blocks
    }

    fn into_note_blocks(self) -> Vec<Block> {
        // A notes page repeats the slide title as its own placeholder; only
        // the note body is the speaker's note.
        self.notes.into_iter().map(Block::paragraph).collect()
    }
}

/// Text placeholder kinds from `TextHeaderAtom` (MS-PPT §2.9.71).
const TEXT_TYPE_TITLE: u32 = 0;
const TEXT_TYPE_NOTES: u32 = 2;
const TEXT_TYPE_CENTER_TITLE: u32 = 6;

fn collect_text(
    stream: &[u8],
    offset: usize,
    budget: &mut Budget,
    out: &mut SlideText,
) -> Result<(), DocumentError> {
    let Some(container) = read_record(stream, offset) else {
        return Ok(());
    };
    let mut text_type = u32::MAX;
    walk(
        stream,
        container.body.start,
        container.body.end,
        budget,
        &mut |record, stream| {
            let body = &stream[record.body.clone()];
            match record.kind {
                RT_TEXT_HEADER_ATOM => {
                    text_type = if body.len() >= 4 {
                        read_u32(body, 0)
                    } else {
                        3
                    };
                }
                RT_TEXT_CHARS_ATOM => {
                    let text: String = body
                        .chunks_exact(2)
                        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                        .collect::<Vec<_>>()
                        .chunks(1)
                        .flat_map(|unit| {
                            char::decode_utf16(unit.iter().copied())
                                .map(|value| value.unwrap_or(char::REPLACEMENT_CHARACTER))
                        })
                        .collect();
                    push_text(&text, text_type, out);
                }
                RT_TEXT_BYTES_ATOM => {
                    // Each byte is the low half of a UTF-16 code unit whose
                    // high half is zero, so this is Latin-1 by construction.
                    let text: String = body.iter().map(|byte| *byte as char).collect();
                    push_text(&text, text_type, out);
                }
                _ => {}
            }
            record.version == CONTAINER_VERSION
        },
    );
    Ok(())
}

fn push_text(text: &str, text_type: u32, out: &mut SlideText) {
    let normalized = text.replace(LINE_BREAK, "\n");
    let paragraphs: Vec<String> = normalized
        .split(PARAGRAPH_MARK)
        .map(|part| part.trim().to_owned())
        .filter(|part| !part.is_empty())
        .collect();
    if paragraphs.is_empty() {
        return;
    }
    if text_type == TEXT_TYPE_NOTES {
        out.notes.extend(paragraphs);
        return;
    }
    if matches!(text_type, TEXT_TYPE_TITLE | TEXT_TYPE_CENTER_TITLE) && out.title.is_none() {
        let mut paragraphs = paragraphs;
        out.title = Some(paragraphs.remove(0));
        out.paragraphs.extend(paragraphs);
        return;
    }
    out.paragraphs.extend(paragraphs);
}

fn read_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(kind: u16, version: u16, instance: u16, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&((instance << 4) | version).to_le_bytes());
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn text_bytes_atoms_split_on_paragraph_marks() {
        let mut out = SlideText::default();
        push_text("First\rSecond", 1, &mut out);
        assert_eq!(out.paragraphs, vec!["First", "Second"]);
        assert!(out.title.is_none());
    }

    #[test]
    fn a_title_placeholder_becomes_the_slide_heading() {
        let mut out = SlideText::default();
        push_text("Quarterly Review", TEXT_TYPE_TITLE, &mut out);
        push_text("Revenue grew", 1, &mut out);
        assert_eq!(out.title.as_deref(), Some("Quarterly Review"));
        assert_eq!(out.paragraphs, vec!["Revenue grew"]);
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
}
