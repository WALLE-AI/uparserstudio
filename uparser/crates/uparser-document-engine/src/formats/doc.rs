//! Legacy binary Word (`.doc`, MS-DOC).
//!
//! Scope is **content recovery**, not format fidelity: the goal is to get the
//! document's text, paragraph boundaries and table structure out without
//! LibreOffice. MS-DOC's full surface (SPRM-driven character/paragraph
//! properties, fields, drawing objects) is vastly larger than what a text
//! extractor needs, and every unimplemented corner degrades to plain text
//! rather than to a failure.
//!
//! The pipeline follows MS-DOC §2.3 and §2.8:
//!
//!   OLE2 compound file
//!     → `WordDocument` stream  (the FIB, then raw text)
//!     → FIB selects `0Table` or `1Table` as the table stream
//!     → `fcClx`/`lcbClx` locate the CLX inside that stream
//!     → the CLX's piece table maps character positions to byte ranges,
//!       each flagged as 8-bit (codepage) or 16-bit (UTF-16LE)
//!     → concatenating the pieces reconstructs the document text
//!
//! The piece table is what makes a hand-rolled reader necessary at all: Word
//! does not store text contiguously, and a naive "read the stream as UTF-16"
//! extractor returns interleaved garbage on any document that has been edited.

use crate::{
    Block, CanonicalDocument, Cell, CellSlot, CellValueKind, DocumentError, DocumentFormat,
    DocumentUnit, Inline, ParseOptions, ParseWarning, Table, TableKind, UnitKind, WarningCode,
};
use encoding_rs::WINDOWS_1252;
use std::io::{Cursor, Read};

/// Word's own marker characters inside the text stream (MS-DOC §2.8.25).
const PARAGRAPH_MARK: char = '\r';
/// Ends a table cell, and — when the paragraph is the row's last — the row.
const CELL_MARK: char = '\u{7}';
const LINE_BREAK: char = '\u{b}';
const PAGE_BREAK: char = '\u{c}';
const FIELD_BEGIN: char = '\u{13}';
const FIELD_SEPARATOR: char = '\u{14}';
const FIELD_END: char = '\u{15}';

pub(crate) fn parse(
    bytes: &[u8],
    options: &ParseOptions,
) -> Result<CanonicalDocument, DocumentError> {
    let mut compound = cfb::CompoundFile::open(Cursor::new(bytes))
        .map_err(|error| DocumentError::malformed(format!("not an OLE2 container: {error}")))?;

    let word_stream = read_stream(&mut compound, "/WordDocument", options)?.ok_or_else(|| {
        DocumentError::MissingPart {
            part: "WordDocument".to_owned(),
        }
    })?;
    let fib = Fib::parse(&word_stream)?;

    let table_name = if fib.uses_table_1 {
        "/1Table"
    } else {
        "/0Table"
    };
    // Word 6/95 documents have no separate table stream; their text is
    // stored contiguously and the piece table is absent.
    let table_stream: Vec<u8> =
        read_stream(&mut compound, table_name, options)?.unwrap_or_default();

    let mut document = CanonicalDocument::new(DocumentFormat::Doc);
    document.metadata.variant = Some("doc".to_owned());

    let text = extract_text(&fib, &word_stream, &table_stream, &mut document.warnings)?;
    if text.trim().is_empty() {
        return Err(DocumentError::Malformed {
            part: Some("WordDocument".to_owned()),
            detail: "no recoverable text in the document stream".to_owned(),
        });
    }
    enforce_text_budget(text.len(), options)?;

    let mut unit = DocumentUnit::new(UnitKind::Flow, 0, None);
    unit.blocks = blocks_from_text(&text, options)?;
    document.units.push(unit);
    document.warnings.push(ParseWarning {
        code: WarningCode::UnsupportedFeature,
        part: None,
        message: "legacy .doc is recovered as text, paragraphs and tables; \
                  character/paragraph styling, fields and drawings are not retained"
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

/// The parts of the File Information Block this extractor needs.
struct Fib {
    uses_table_1: bool,
    /// Character count of the main document text, which is the prefix of the
    /// piece table we care about — footnotes, headers and annotations follow
    /// it in the same character-position space.
    main_text_length: u32,
    clx_offset: u32,
    clx_length: u32,
    /// `fcMin`/`fcMac`: the contiguous text range used by Word 6/95, and by
    /// any writer that emits a minimal FIB with no piece table.
    fc_min: u32,
    fc_mac: u32,
    /// Install language, which is the only hint a pre-Unicode document gives
    /// about the codepage its 8-bit text is in.
    lid: u16,
    /// `fFarEast`: the writer was an East Asian build, so 8-bit text is
    /// double-byte even when `lid` says nothing.
    far_east: bool,
}

impl Fib {
    fn parse(stream: &[u8]) -> Result<Self, DocumentError> {
        // FibBase is 32 bytes and always present.
        if stream.len() < 34 {
            return Err(DocumentError::malformed("WordDocument stream is truncated"));
        }
        if read_u16(stream, 0) != 0xA5EC {
            return Err(DocumentError::malformed(
                "WordDocument stream has no FIB signature",
            ));
        }
        // FibBase.flags bit 9 selects which table stream is current; writing
        // alternates between them, so reading the wrong one yields a stale
        // piece table and scrambled text.
        let uses_table_1 = read_u16(stream, 10) & 0x0200 != 0;
        let lid = read_u16(stream, 6);
        let far_east = read_u16(stream, 10) & 0x4000 != 0;
        // Word 6/95 kept the contiguous text range here. Word 97+ leaves the
        // fields in place, and a writer that emits a minimal FIB (no piece
        // table at all) still fills them in.
        let fc_min = read_u32(stream, 24);
        let fc_mac = read_u32(stream, 28);

        // Variable-length sections follow FibBase; each is preceded by its own
        // count, so the offsets have to be walked rather than assumed.
        let csw = read_u16(stream, 32) as usize;
        let fib_rg_lw_count_at = 34 + csw * 2;
        let cslw = read_u16_checked(stream, fib_rg_lw_count_at).unwrap_or(0) as usize;
        let fib_rg_lw_at = fib_rg_lw_count_at + 2;

        // FibRgLw97 index 3 is ccpText (MS-DOC §2.5.4).
        let main_text_length = if cslw > 3 {
            read_u32_checked(stream, fib_rg_lw_at + 3 * 4)?
        } else {
            0
        };

        let fc_lcb_count_at = fib_rg_lw_at + cslw * 4;
        let fc_lcb_count = read_u16_checked(stream, fc_lcb_count_at).unwrap_or(0) as usize;
        let fc_lcb_at = fc_lcb_count_at + 2;

        // FibRgFcLcb97 pair 33 is fcClx/lcbClx (MS-DOC §2.5.5). A FIB too
        // short to reach it has no piece table; the contiguous `fcMin`/`fcMac`
        // range is then the whole document, so this is a fallback rather than
        // a failure.
        const CLX_PAIR: usize = 33;
        let (clx_offset, clx_length) = if fc_lcb_count > CLX_PAIR {
            (
                read_u32_checked(stream, fc_lcb_at + CLX_PAIR * 8)?,
                read_u32_checked(stream, fc_lcb_at + CLX_PAIR * 8 + 4)?,
            )
        } else {
            (0, 0)
        };

        Ok(Self {
            uses_table_1,
            main_text_length,
            clx_offset,
            clx_length,
            fc_min,
            fc_mac,
            lid,
            far_east,
        })
    }
}

/// One run of text, as located by the piece table.
struct Piece {
    /// Character position where this piece starts.
    start_cp: u32,
    end_cp: u32,
    /// Byte offset into the WordDocument stream.
    offset: u32,
    /// 8-bit codepage text rather than UTF-16LE.
    compressed: bool,
}

fn extract_text(
    fib: &Fib,
    word_stream: &[u8],
    table_stream: &[u8],
    warnings: &mut Vec<ParseWarning>,
) -> Result<String, DocumentError> {
    let pieces = if fib.clx_length > 0 {
        parse_piece_table(fib, table_stream, warnings)?
    } else {
        Vec::new()
    };
    if pieces.is_empty() {
        return Ok(contiguous_text(fib, word_stream));
    }

    let mut text = String::new();
    for piece in pieces {
        // Only the main document body; footnotes/headers live beyond it in
        // the same character-position space and are not part of the body.
        if fib.main_text_length > 0 && piece.start_cp >= fib.main_text_length {
            break;
        }
        let end_cp = if fib.main_text_length > 0 {
            piece.end_cp.min(fib.main_text_length)
        } else {
            piece.end_cp
        };
        let characters = end_cp.saturating_sub(piece.start_cp) as usize;
        if characters == 0 {
            continue;
        }

        if piece.compressed {
            let start = piece.offset as usize;
            let Some(slice) = word_stream.get(start..start + characters) else {
                warnings.push(truncated_piece());
                break;
            };
            // A compressed piece is codepage text with a handful of
            // Word-specific substitutions in the 0x80-0x9F range that
            // WINDOWS_1252 already maps to the intended characters.
            let (decoded, _, _) = WINDOWS_1252.decode(slice);
            text.push_str(&decoded);
        } else {
            let start = piece.offset as usize;
            let Some(slice) = word_stream.get(start..start + characters * 2) else {
                warnings.push(truncated_piece());
                break;
            };
            text.extend(
                slice
                    .chunks_exact(2)
                    .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                    .collect::<Vec<_>>()
                    .chunks(1)
                    .flat_map(|unit| {
                        char::decode_utf16(unit.iter().copied())
                            .map(|value| value.unwrap_or(char::REPLACEMENT_CHARACTER))
                    }),
            );
        }
    }
    Ok(text)
}

/// Word 6/95 layout: the body is one contiguous 8-bit run between `fcMin` and
/// `fcMac`, with no piece table to consult.
fn contiguous_text(fib: &Fib, word_stream: &[u8]) -> String {
    let start = fib.fc_min as usize;
    let end = (fib.fc_mac as usize).min(word_stream.len());
    if start >= end {
        return String::new();
    }
    let bytes = &word_stream[start..end];
    // `lid` is the primary signal, but a document written by an East Asian
    // build can carry `lid = 0` while still holding double-byte text; the
    // `fFarEast` flag is then the only marker, and it does not say *which*
    // East Asian codepage. Decoding against each candidate and keeping the
    // one that round-trips without replacement characters resolves it from
    // the data instead of guessing.
    if fib.lid & 0x03FF == 0 && fib.far_east {
        for candidate in [
            encoding_rs::SHIFT_JIS,
            encoding_rs::GBK,
            encoding_rs::BIG5,
            encoding_rs::EUC_KR,
        ] {
            let (decoded, _, had_errors) = candidate.decode(bytes);
            if !had_errors {
                return decoded.into_owned();
            }
        }
    }
    let (decoded, _, _) = codepage_for(fib.lid).decode(bytes);
    decoded.into_owned()
}

/// Best-effort codepage for a pre-Unicode document.
///
/// The install language is the only signal the format carries; getting it
/// wrong turns Cyrillic or Japanese text into mojibake, which is worse than
/// no text at all, so the mapping covers the languages whose codepages are
/// not Latin-1 compatible and treats everything else as Windows-1252.
fn codepage_for(lid: u16) -> &'static encoding_rs::Encoding {
    use encoding_rs::{
        BIG5, EUC_KR, GBK, SHIFT_JIS, WINDOWS_1250, WINDOWS_1251, WINDOWS_1253, WINDOWS_1254,
        WINDOWS_1255, WINDOWS_1256, WINDOWS_1257,
    };
    // The low 10 bits are the primary language; sub-language does not change
    // the codepage.
    match lid & 0x03FF {
        0x11 => SHIFT_JIS, // Japanese
        0x12 => EUC_KR,    // Korean
        // Cyrillic: Russian, Bulgarian, Ukrainian, Belarusian, Macedonian.
        0x19 | 0x02 | 0x22 | 0x23 | 0x2F => WINDOWS_1251,
        // Central European: Czech, Hungarian, Polish, Romanian, Croatian,
        // Slovak, Albanian, Slovenian.
        0x05 | 0x0E | 0x15 | 0x18 | 0x1A | 0x1B | 0x1C | 0x24 => WINDOWS_1250,
        0x08 => WINDOWS_1253, // Greek
        0x1F => WINDOWS_1254, // Turkish
        0x0D => WINDOWS_1255, // Hebrew
        // Arabic, Farsi, Urdu.
        0x01 | 0x29 | 0x20 => WINDOWS_1256,
        // Baltic: Estonian, Latvian, Lithuanian.
        0x25..=0x27 => WINDOWS_1257,
        // Chinese: Traditional and Simplified use different codepages, and
        // only the sub-language distinguishes them.
        0x04 if matches!(lid, 0x0404 | 0x0C04 | 0x1404) => BIG5,
        0x04 => GBK,
        _ => WINDOWS_1252,
    }
}

fn truncated_piece() -> ParseWarning {
    ParseWarning {
        code: WarningCode::TruncatedContent,
        part: Some("WordDocument".to_owned()),
        message: "piece table references text past the end of the stream".to_owned(),
    }
}

/// Walk the CLX to its `Pcdt`, then read the `PlcPcd` inside it.
///
/// A CLX is a sequence of `Prc` entries (property modifiers, skipped here)
/// terminated by the single `Pcdt` that holds the piece table.
fn parse_piece_table(
    fib: &Fib,
    table_stream: &[u8],
    warnings: &mut Vec<ParseWarning>,
) -> Result<Vec<Piece>, DocumentError> {
    let start = fib.clx_offset as usize;
    let end = start.saturating_add(fib.clx_length as usize);
    let Some(clx) = table_stream.get(start..end.min(table_stream.len())) else {
        warnings.push(ParseWarning {
            code: WarningCode::TruncatedContent,
            part: Some("table stream".to_owned()),
            message: "CLX lies outside the table stream".to_owned(),
        });
        return Ok(Vec::new());
    };

    let mut cursor = 0usize;
    while cursor < clx.len() {
        match clx[cursor] {
            // Prc: a 16-bit length followed by that many bytes of property
            // data. Not needed for text, so skipped wholesale.
            0x01 => {
                let length = read_u16_checked(clx, cursor + 1)? as usize;
                cursor = cursor + 3 + length;
            }
            // Pcdt: the piece table itself.
            0x02 => {
                let length = read_u32_checked(clx, cursor + 1)? as usize;
                let body_at = cursor + 5;
                let Some(body) = clx.get(body_at..(body_at + length).min(clx.len())) else {
                    return Ok(Vec::new());
                };
                return Ok(parse_plc_pcd(body));
            }
            _ => break,
        }
    }
    warnings.push(ParseWarning {
        code: WarningCode::TruncatedContent,
        part: Some("table stream".to_owned()),
        message: "CLX contains no piece table".to_owned(),
    });
    Ok(Vec::new())
}

/// `PlcPcd`: `n + 1` character positions followed by `n` 8-byte descriptors.
///
/// The count is recovered from the total length because it is not stored:
/// `len = 4*(n+1) + 8*n`, so `n = (len - 4) / 12`.
fn parse_plc_pcd(body: &[u8]) -> Vec<Piece> {
    if body.len() < 16 {
        return Vec::new();
    }
    let count = (body.len() - 4) / 12;
    let descriptors_at = 4 * (count + 1);
    let mut pieces = Vec::with_capacity(count);

    for index in 0..count {
        let start_cp = read_u32(body, index * 4);
        let end_cp = read_u32(body, (index + 1) * 4);
        if end_cp <= start_cp {
            continue;
        }
        let descriptor_at = descriptors_at + index * 8;
        if descriptor_at + 8 > body.len() {
            break;
        }
        // Pcd: 2 bytes of flags, then the FcCompressed.
        let raw = read_u32(body, descriptor_at + 2);
        // Bit 30 marks 8-bit text; when set the real byte offset is half the
        // stored value (MS-DOC §2.9.73).
        let compressed = raw & 0x4000_0000 != 0;
        let offset = if compressed {
            (raw & 0x3FFF_FFFF) / 2
        } else {
            raw & 0x3FFF_FFFF
        };
        pieces.push(Piece {
            start_cp,
            end_cp,
            offset,
            compressed,
        });
    }
    pieces
}

/// Turn Word's flat marker-separated text into blocks.
///
/// Paragraphs end at `\r`. A paragraph ending in `\u{7}` is a table cell, and
/// the row ends at the cell mark that carries the row's own paragraph mark —
/// which is why cells are accumulated until a row terminator is seen.
fn blocks_from_text(text: &str, options: &ParseOptions) -> Result<Vec<Block>, DocumentError> {
    let mut blocks = Vec::new();
    let mut paragraph = String::new();
    let mut row: Vec<String> = Vec::new();
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut in_field_instruction = false;
    let mut expanded = 0u64;

    let flush_paragraph = |paragraph: &mut String, blocks: &mut Vec<Block>| {
        let trimmed = paragraph.trim();
        if !trimmed.is_empty() {
            blocks.push(Block::paragraph(trimmed));
        }
        paragraph.clear();
    };

    for character in text.chars() {
        match character {
            // Field instructions are machinery (`HYPERLINK "…"`), not content;
            // the result that follows the separator is the visible text.
            FIELD_BEGIN => in_field_instruction = true,
            FIELD_SEPARATOR => in_field_instruction = false,
            FIELD_END => in_field_instruction = false,
            _ if in_field_instruction => {}
            CELL_MARK => {
                row.push(std::mem::take(&mut paragraph).trim().to_owned());
                expanded += 1;
                if expanded > options.limits.max_expansion {
                    return Err(DocumentError::ResourceLimit {
                        limit: "max_expansion",
                        detail: "legacy .doc table expands beyond the configured budget".to_owned(),
                    });
                }
            }
            PARAGRAPH_MARK => {
                // A paragraph mark immediately after cell marks terminates the
                // row; otherwise any pending row is complete and the table
                // ends here.
                if !row.is_empty() {
                    rows.push(std::mem::take(&mut row));
                    continue;
                }
                if !rows.is_empty() {
                    blocks.push(table_block(std::mem::take(&mut rows)));
                }
                flush_paragraph(&mut paragraph, &mut blocks);
            }
            LINE_BREAK => paragraph.push('\n'),
            PAGE_BREAK => {
                flush_paragraph(&mut paragraph, &mut blocks);
                blocks.push(Block::Rule);
            }
            // Remaining control characters are anchors for drawings, notes and
            // similar structures with no textual content of their own.
            character if (character as u32) < 0x20 && character != '\t' => {}
            character => paragraph.push(character),
        }
    }

    if !row.is_empty() {
        rows.push(row);
    }
    if !rows.is_empty() {
        blocks.push(table_block(rows));
    }
    flush_paragraph(&mut paragraph, &mut blocks);
    Ok(blocks)
}

fn table_block(rows: Vec<Vec<String>>) -> Block {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let grid: Vec<Vec<CellSlot>> = rows
        .iter()
        .map(|row| {
            (0..columns)
                .map(|column| {
                    let text = row.get(column).map(String::as_str).unwrap_or_default();
                    CellSlot::Origin(Cell::text(
                        text,
                        if text.is_empty() {
                            CellValueKind::Empty
                        } else {
                            CellValueKind::Text
                        },
                    ))
                })
                .collect()
        })
        .collect();
    // The binary format records merges through SPRMs this extractor does not
    // read, so every cell is its own origin.
    Block::Table {
        table: Table {
            kind: TableKind::Data,
            rows: grid.len(),
            columns,
            header_rows: 0,
            grid,
            caption: None::<Vec<Inline>>,
        },
    }
}

fn enforce_text_budget(len: usize, options: &ParseOptions) -> Result<(), DocumentError> {
    if len > options.limits.max_text_bytes {
        return Err(DocumentError::ResourceLimit {
            limit: "max_text_bytes",
            detail: format!("recovered {len} bytes of text"),
        });
    }
    Ok(())
}

fn read_u16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn read_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn read_u16_checked(bytes: &[u8], at: usize) -> Result<u16, DocumentError> {
    bytes
        .get(at..at + 2)
        .map(|slice| u16::from_le_bytes([slice[0], slice[1]]))
        .ok_or_else(|| DocumentError::malformed("legacy Word structure is truncated"))
}

fn read_u32_checked(bytes: &[u8], at: usize) -> Result<u32, DocumentError> {
    bytes
        .get(at..at + 4)
        .map(|slice| u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
        .ok_or_else(|| DocumentError::malformed("legacy Word structure is truncated"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fib_bytes(
        flags: u16,
        lid: u16,
        fc_min: u32,
        fc_mac: u32,
        main_text_length: u32,
        clx_offset: u32,
        clx_length: u32,
    ) -> Vec<u8> {
        let mut bytes = vec![0u8; 326];
        bytes[0..2].copy_from_slice(&0xA5ECu16.to_le_bytes());
        bytes[6..8].copy_from_slice(&lid.to_le_bytes());
        bytes[10..12].copy_from_slice(&flags.to_le_bytes());
        bytes[24..28].copy_from_slice(&fc_min.to_le_bytes());
        bytes[28..32].copy_from_slice(&fc_mac.to_le_bytes());
        bytes[32..34].copy_from_slice(&0u16.to_le_bytes());
        bytes[34..36].copy_from_slice(&4u16.to_le_bytes());
        bytes[48..52].copy_from_slice(&main_text_length.to_le_bytes());
        bytes[52..54].copy_from_slice(&34u16.to_le_bytes());
        bytes[318..322].copy_from_slice(&clx_offset.to_le_bytes());
        bytes[322..326].copy_from_slice(&clx_length.to_le_bytes());
        bytes
    }

    fn plc_pcd(cps: &[u32], descriptors: &[(u32, bool)]) -> Vec<u8> {
        assert_eq!(cps.len(), descriptors.len() + 1);
        let mut body = Vec::new();
        for cp in cps {
            body.extend_from_slice(&cp.to_le_bytes());
        }
        for &(offset, compressed) in descriptors {
            body.extend_from_slice(&0u16.to_le_bytes());
            let raw = if compressed {
                0x4000_0000 | offset.saturating_mul(2)
            } else {
                offset
            };
            body.extend_from_slice(&raw.to_le_bytes());
            body.extend_from_slice(&0u16.to_le_bytes());
        }
        body
    }

    fn clx(body: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0x01, 2, 0, 0xAA, 0xBB, 0x02];
        bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
        bytes.extend_from_slice(body);
        bytes
    }

    fn ole(streams: &[(&str, &[u8])]) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut compound = cfb::CompoundFile::create(Cursor::new(&mut bytes)).unwrap();
            for &(name, contents) in streams {
                let mut stream = compound.create_stream(name).unwrap();
                stream.write_all(contents).unwrap();
            }
            compound.flush().unwrap();
        }
        bytes
    }

    #[test]
    fn paragraph_marks_split_blocks() {
        let blocks = blocks_from_text("First\rSecond\r", &ParseOptions::default()).unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0], Block::paragraph("First"));
        assert_eq!(blocks[1], Block::paragraph("Second"));
    }

    #[test]
    fn cell_marks_build_a_table() {
        // Two cells then a row terminator, twice.
        let blocks =
            blocks_from_text("A\u{7}B\u{7}\rC\u{7}D\u{7}\r", &ParseOptions::default()).unwrap();
        let Block::Table { table } = &blocks[0] else {
            panic!("{blocks:?}")
        };
        assert_eq!((table.rows, table.columns), (2, 2));
        let CellSlot::Origin(cell) = &table.grid[1][1] else {
            panic!()
        };
        assert_eq!(cell.blocks, vec![Block::paragraph("D")]);
    }

    #[test]
    fn field_instructions_are_dropped_but_results_are_kept() {
        // `\u{13}` instruction `\u{14}` result `\u{15}`.
        let blocks = blocks_from_text(
            "See \u{13}HYPERLINK \"http://x\"\u{14}the site\u{15} now\r",
            &ParseOptions::default(),
        )
        .unwrap();
        assert_eq!(blocks[0], Block::paragraph("See the site now"));
    }

    #[test]
    fn a_stream_without_the_fib_signature_is_rejected() {
        assert!(matches!(
            Fib::parse(&[0u8; 64]),
            Err(DocumentError::Malformed { .. })
        ));
    }

    #[test]
    fn oversized_table_expansion_is_a_resource_limit() {
        let mut options = ParseOptions::default();
        options.limits.max_expansion = 4;
        let text = "a\u{7}b\u{7}c\u{7}d\u{7}e\u{7}f\u{7}\r";
        assert!(matches!(
            blocks_from_text(text, &options),
            Err(DocumentError::ResourceLimit {
                limit: "max_expansion",
                ..
            })
        ));
    }

    #[test]
    fn fib_walks_variable_sections_and_reads_flags() {
        let bytes = fib_bytes(0x4200, 0x0411, 400, 450, 9, 17, 23);
        let fib = Fib::parse(&bytes).unwrap();
        assert!(fib.uses_table_1);
        assert!(fib.far_east);
        assert_eq!(fib.lid, 0x0411);
        assert_eq!((fib.fc_min, fib.fc_mac), (400, 450));
        assert_eq!(fib.main_text_length, 9);
        assert_eq!((fib.clx_offset, fib.clx_length), (17, 23));
    }

    #[test]
    fn short_fib_variants_fall_back_or_fail_cleanly() {
        assert!(matches!(
            Fib::parse(&[0u8; 33]),
            Err(DocumentError::Malformed { .. })
        ));

        let mut minimal = vec![0u8; 36];
        minimal[0..2].copy_from_slice(&0xA5ECu16.to_le_bytes());
        let fib = Fib::parse(&minimal).unwrap();
        assert_eq!(fib.main_text_length, 0);
        assert_eq!((fib.clx_offset, fib.clx_length), (0, 0));

        minimal[34..36].copy_from_slice(&4u16.to_le_bytes());
        assert!(matches!(
            Fib::parse(&minimal),
            Err(DocumentError::Malformed { .. })
        ));
    }

    #[test]
    fn piece_table_decodes_compressed_and_utf16_text() {
        let body = plc_pcd(&[0, 3, 5], &[(400, true), (500, false)]);
        let table = clx(&body);
        let fib = Fib::parse(&fib_bytes(0, 0x0409, 0, 0, 5, 0, table.len() as u32)).unwrap();
        let mut word = vec![0u8; 504];
        word[400..403].copy_from_slice(b"Hi ");
        word[500..504].copy_from_slice(&[0x16, 0x4E, 0x4C, 0x75]);
        let mut warnings = Vec::new();
        assert_eq!(
            extract_text(&fib, &word, &table, &mut warnings).unwrap(),
            "Hi 世界"
        );
        assert!(warnings.is_empty());
    }

    #[test]
    fn main_text_length_clips_and_skips_later_pieces() {
        let body = plc_pcd(&[0, 4, 8], &[(100, true), (110, true)]);
        let table = clx(&body);
        let fib = Fib::parse(&fib_bytes(0, 0x0409, 0, 0, 2, 0, table.len() as u32)).unwrap();
        let mut word = vec![0u8; 114];
        word[100..104].copy_from_slice(b"body");
        word[110..114].copy_from_slice(b"tail");
        assert_eq!(
            extract_text(&fib, &word, &table, &mut Vec::new()).unwrap(),
            "bo"
        );
    }

    #[test]
    fn invalid_or_truncated_pieces_degrade_with_warnings() {
        let body = plc_pcd(&[0, 4], &[(100, true)]);
        let table = clx(&body);
        let fib = Fib::parse(&fib_bytes(0, 0, 1, 4, 0, 0, table.len() as u32)).unwrap();
        let mut warnings = Vec::new();
        assert_eq!(
            extract_text(&fib, b" fallback", &table, &mut warnings).unwrap(),
            ""
        );
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, WarningCode::TruncatedContent);

        let outside = Fib::parse(&fib_bytes(0, 0, 1, 4, 0, 999, 10)).unwrap();
        warnings.clear();
        assert_eq!(
            extract_text(&outside, b" fallback", b"tiny", &mut warnings).unwrap(),
            "fal"
        );
        assert!(warnings[0].message.contains("outside"));
    }

    #[test]
    fn malformed_clx_and_plc_are_handled() {
        let fib = Fib::parse(&fib_bytes(0, 0, 0, 0, 0, 0, 4)).unwrap();
        assert!(matches!(
            parse_piece_table(&fib, &[0x01, 0xFF], &mut Vec::new()),
            Err(DocumentError::Malformed { .. })
        ));

        let mut warnings = Vec::new();
        assert!(
            parse_piece_table(&fib, &[0, 0, 0, 0], &mut warnings)
                .unwrap()
                .is_empty()
        );
        assert!(warnings[0].message.contains("no piece table"));
        assert!(parse_plc_pcd(&[0; 15]).is_empty());

        let invalid_cp = plc_pcd(&[5, 5], &[(20, true)]);
        assert!(parse_plc_pcd(&invalid_cp).is_empty());
    }

    #[test]
    fn contiguous_text_uses_language_codepages_and_bounds() {
        let cyrillic = Fib::parse(&fib_bytes(0, 0x0419, 1, 7, 0, 0, 0)).unwrap();
        assert_eq!(
            contiguous_text(&cyrillic, b" \xCF\xF0\xE8\xE2\xE5\xF2"),
            "Привет"
        );

        let japanese = Fib::parse(&fib_bytes(0x4000, 0, 0, 4, 0, 0, 0)).unwrap();
        assert_eq!(
            contiguous_text(&japanese, &[0x93, 0xFA, 0x96, 0x7B]),
            "日本"
        );

        let empty = Fib::parse(&fib_bytes(0, 0, 10, 2, 0, 0, 0)).unwrap();
        assert!(contiguous_text(&empty, b"short").is_empty());
    }

    #[test]
    fn codepage_mapping_covers_language_families() {
        assert_eq!(codepage_for(0x0411).name(), "Shift_JIS");
        assert_eq!(codepage_for(0x0412).name(), "EUC-KR");
        assert_eq!(codepage_for(0x0419).name(), "windows-1251");
        assert_eq!(codepage_for(0x0405).name(), "windows-1250");
        assert_eq!(codepage_for(0x0408).name(), "windows-1253");
        assert_eq!(codepage_for(0x041F).name(), "windows-1254");
        assert_eq!(codepage_for(0x040D).name(), "windows-1255");
        assert_eq!(codepage_for(0x0401).name(), "windows-1256");
        assert_eq!(codepage_for(0x0425).name(), "windows-1257");
        assert_eq!(codepage_for(0x0404).name(), "Big5");
        assert_eq!(codepage_for(0x0804).name(), "GBK");
        assert_eq!(codepage_for(0).name(), "windows-1252");
    }

    #[test]
    fn block_conversion_handles_breaks_controls_and_uneven_tables() {
        let blocks = blocks_from_text(
            "one\u{b}two\u{c}A\u{7}B\u{7}\rC\u{7}\r\u{1}tail",
            &ParseOptions::default(),
        )
        .unwrap();
        assert_eq!(blocks[0], Block::paragraph("one\ntwo"));
        assert_eq!(blocks[1], Block::Rule);
        let Block::Table { table } = &blocks[2] else {
            panic!("{blocks:?}")
        };
        assert_eq!((table.rows, table.columns), (2, 2));
        let CellSlot::Origin(empty) = &table.grid[1][1] else {
            panic!()
        };
        assert_eq!(empty.value_kind, CellValueKind::Empty);
        assert_eq!(blocks[3], Block::paragraph("tail"));

        let trailing = blocks_from_text("x\u{7}y", &ParseOptions::default()).unwrap();
        assert!(matches!(trailing[0], Block::Table { .. }));
    }

    #[test]
    fn text_budget_is_enforced() {
        let mut options = ParseOptions::default();
        options.limits.max_text_bytes = 3;
        assert!(matches!(
            enforce_text_budget(4, &options),
            Err(DocumentError::ResourceLimit {
                limit: "max_text_bytes",
                ..
            })
        ));
        enforce_text_budget(3, &options).unwrap();
    }

    #[test]
    fn real_ole_parse_uses_selected_table_stream() {
        let body = plc_pcd(&[0, 6], &[(400, true)]);
        let table = clx(&body);
        let mut word = fib_bytes(0x0200, 0x0409, 0, 0, 6, 0, table.len() as u32);
        word.resize(406, 0);
        word[400..406].copy_from_slice(b"Hello\r");
        let bytes = ole(&[("WordDocument", &word), ("1Table", &table)]);
        let document = parse(&bytes, &ParseOptions::default()).unwrap();
        assert_eq!(document.metadata.format, DocumentFormat::Doc);
        assert_eq!(document.units[0].blocks[0], Block::paragraph("Hello"));
        assert_eq!(document.warnings.len(), 1);
    }

    #[test]
    fn ole_parse_reports_missing_stream_and_entry_limit() {
        let bytes = ole(&[("Other", b"data")]);
        assert!(matches!(
            parse(&bytes, &ParseOptions::default()),
            Err(DocumentError::MissingPart { .. })
        ));

        let word = fib_bytes(0, 0x0409, 0, 0, 0, 0, 0);
        let bytes = ole(&[("WordDocument", &word)]);
        let mut options = ParseOptions::default();
        options.limits.max_entry_bytes = 10;
        assert!(matches!(
            parse(&bytes, &options),
            Err(DocumentError::ResourceLimit {
                limit: "max_entry_bytes",
                ..
            })
        ));
    }
}
