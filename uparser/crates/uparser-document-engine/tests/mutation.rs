//! Deterministic mutation corpus.
//!
//! Every format frontend consumes untrusted bytes. The hand-written tests in
//! `lib.rs` cover the shapes we thought of; this covers the ones we did not,
//! by taking each well-formed fixture apart in a reproducible way and
//! asserting the only two acceptable outcomes: a parsed document, or a typed
//! [`DocumentError`]. A panic, a hang, or an unbounded allocation is a bug.
//!
//! Mutations are driven by a fixed-seed LCG rather than a random source, so a
//! failure names an exact `(fixture, mutation index)` a developer can replay.

use std::io::{Cursor, Write};
use std::time::{Duration, Instant};
use uparser_document_engine::{DocumentError, ParseOptions, parse_document_auto};

/// No single mutated input should take anywhere near this long. The resource
/// limits bound the work; this catches a limit that fails to bind.
const PER_CASE_BUDGET: Duration = Duration::from_secs(5);

/// Mutations per fixture. Enough to exercise every mutation kind at many
/// offsets while keeping the suite fast enough to run on every commit.
const MUTATIONS_PER_FIXTURE: u32 = 200;

/// Deterministic, self-contained PRNG — reproducing a failure must not depend
/// on the platform's RNG or on `rand`'s version.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 16
    }

    fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            0
        } else {
            (self.next() % bound as u64) as usize
        }
    }
}

/// Corrupt `bytes` in one of a few structurally different ways.
fn mutate(bytes: &[u8], rng: &mut Lcg) -> Vec<u8> {
    let mut out = bytes.to_vec();
    if out.is_empty() {
        return out;
    }
    match rng.next() % 6 {
        // Flip a byte: the subtlest corruption, and the one most likely to
        // produce input that still parses part-way.
        0 => {
            let at = rng.below(out.len());
            out[at] ^= 1 << (rng.next() % 8);
        }
        // Truncate: models a partial download or an interrupted write.
        1 => {
            let at = rng.below(out.len());
            out.truncate(at);
        }
        // Splice a slice over itself: produces structurally impossible
        // nesting rather than merely invalid bytes.
        2 => {
            let len = out.len();
            let from = rng.below(len);
            let to = rng.below(len);
            let span = rng.below(len - from.min(len - 1)).min(64);
            let chunk: Vec<u8> = out[from..(from + span).min(len)].to_vec();
            for (offset, byte) in chunk.into_iter().enumerate() {
                if to + offset < len {
                    out[to + offset] = byte;
                }
            }
        }
        // Zero a run: a common effect of a bad sector or a sparse file.
        3 => {
            let at = rng.below(out.len());
            let span = rng.below(64).min(out.len() - at);
            out[at..at + span].fill(0);
        }
        // Insert a byte: shifts every following offset, which upsets any
        // parser that trusts a recorded length.
        4 => {
            let at = rng.below(out.len());
            out.insert(at, (rng.next() % 256) as u8);
        }
        // Delete a byte: the same, in the other direction.
        _ => {
            let at = rng.below(out.len());
            out.remove(at);
        }
    }
    out
}

fn zip_fixture(parts: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, body) in parts {
            writer.start_file(*name, options).unwrap();
            writer.write_all(body).unwrap();
        }
        writer.finish().unwrap();
    }
    buf
}

fn docx() -> Vec<u8> {
    zip_fixture(&[
        (
            "[Content_Types].xml",
            b"<Types><Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/></Types>",
        ),
        (
            "_rels/.rels",
            b"<Relationships><Relationship Id=\"r0\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>",
        ),
        (
            "word/styles.xml",
            b"<w:styles xmlns:w=\"w\"><w:style w:type=\"paragraph\" w:styleId=\"H\"><w:name w:val=\"Heading 1\"/></w:style></w:styles>",
        ),
        (
            "word/numbering.xml",
            b"<w:numbering xmlns:w=\"w\"><w:abstractNum w:abstractNumId=\"1\"><w:lvl w:ilvl=\"0\"><w:start w:val=\"1\"/><w:numFmt w:val=\"decimal\"/></w:lvl></w:abstractNum><w:num w:numId=\"1\"><w:abstractNumId w:val=\"1\"/></w:num></w:numbering>",
        ),
        (
            "word/document.xml",
            b"<w:document xmlns:w=\"w\"><w:body><w:p><w:pPr><w:pStyle w:val=\"H\"/></w:pPr><w:r><w:t>Title</w:t></w:r></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val=\"0\"/><w:numId w:val=\"1\"/></w:numPr></w:pPr><w:r><w:t>item</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:tcPr><w:gridSpan w:val=\"2\"/></w:tcPr><w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:body></w:document>",
        ),
    ])
}

fn pptx() -> Vec<u8> {
    zip_fixture(&[
        (
            "[Content_Types].xml",
            b"<Types><Override PartName=\"/ppt/presentation.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml\"/></Types>",
        ),
        (
            "_rels/.rels",
            b"<Relationships><Relationship Id=\"r0\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"ppt/presentation.xml\"/></Relationships>",
        ),
        (
            "ppt/presentation.xml",
            b"<p:presentation xmlns:p=\"p\" xmlns:r=\"r\"><p:sldIdLst><p:sldId id=\"256\" r:id=\"rS\"/></p:sldIdLst></p:presentation>",
        ),
        (
            "ppt/_rels/presentation.xml.rels",
            b"<Relationships><Relationship Id=\"rS\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide\" Target=\"slides/slide1.xml\"/></Relationships>",
        ),
        (
            "ppt/slides/slide1.xml",
            b"<p:sld xmlns:p=\"p\" xmlns:a=\"a\"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:nvPr><p:ph type=\"title\"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>Deck</a:t></a:r></a:p></p:txBody></p:sp><p:graphicFrame><a:graphic><a:graphicData><a:tbl><a:tr><a:tc gridSpan=\"2\"><a:txBody><a:p><a:r><a:t>x</a:t></a:r></a:p></a:txBody></a:tc></a:tr></a:tbl></a:graphicData></a:graphic></p:graphicFrame></p:spTree></p:cSld></p:sld>",
        ),
    ])
}

fn odt() -> Vec<u8> {
    zip_fixture(&[
        ("mimetype", b"application/vnd.oasis.opendocument.text"),
        (
            "styles.xml",
            b"<office:document-styles xmlns:office=\"office\" xmlns:text=\"text\" xmlns:style=\"style\"><office:styles><text:list-style style:name=\"L\"><text:list-level-style-number text:level=\"1\" style:num-format=\"a\"/></text:list-style></office:styles></office:document-styles>",
        ),
        (
            "content.xml",
            b"<office:document-content xmlns:office=\"office\" xmlns:text=\"text\" xmlns:table=\"table\" xmlns:style=\"style\" xmlns:fo=\"fo\"><office:automatic-styles><style:style style:name=\"T\" style:family=\"text\"><style:text-properties fo:font-weight=\"bold\"/></style:style></office:automatic-styles><office:body><office:text><text:h text:outline-level=\"1\">H</text:h><text:p><text:span text:style-name=\"T\">bold</text:span></text:p><text:list text:style-name=\"L\"><text:list-item><text:p>a</text:p></text:list-item></text:list><table:table table:name=\"T1\"><table:table-row><table:table-cell table:number-columns-spanned=\"2\"><text:p>c</text:p></table:table-cell><table:covered-table-cell/></table:table-row></table:table></office:text></office:body></office:document-content>",
        ),
    ])
}

fn ods() -> Vec<u8> {
    zip_fixture(&[
        (
            "mimetype",
            b"application/vnd.oasis.opendocument.spreadsheet",
        ),
        (
            "content.xml",
            b"<office:document-content xmlns:office=\"office\" xmlns:table=\"table\" xmlns:text=\"text\"><office:body><office:spreadsheet><table:table table:name=\"S\"><table:table-row table:number-rows-repeated=\"3\"><table:table-cell table:number-columns-repeated=\"2\"><text:p>v</text:p></table:table-cell></table:table-row></table:table></office:spreadsheet></office:body></office:document-content>",
        ),
    ])
}

fn epub() -> Vec<u8> {
    zip_fixture(&[
        ("mimetype", b"application/epub+zip"),
        (
            "META-INF/container.xml",
            b"<container><rootfiles><rootfile full-path=\"b.opf\" media-type=\"application/oebps-package+xml\"/></rootfiles></container>",
        ),
        (
            "b.opf",
            b"<package xmlns:dc=\"dc\"><metadata><dc:title>T</dc:title></metadata><manifest><item id=\"c\" href=\"c.xhtml\" media-type=\"application/xhtml+xml\"/></manifest><spine><itemref idref=\"c\"/></spine></package>",
        ),
        (
            "c.xhtml",
            b"<html xmlns=\"http://www.w3.org/1999/xhtml\"><body><h1>H</h1><p><strong>b</strong></p><ul><li>x<ul><li>y</li></ul></li></ul><table><tr><th>A</th></tr><tr><td rowspan=\"2\">B</td></tr></table></body></html>",
        ),
    ])
}

/// A minimal but genuine OLE2 compound file. The binary frontends index into
/// raw byte structures, so they must be fuzzed against a real container —
/// a `PK`-style stub would be rejected before reaching any of that code.
fn ole(streams: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut compound = cfb::CompoundFile::create(Cursor::new(&mut buf)).unwrap();
        for (name, body) in streams {
            let mut stream = compound.create_stream(name).unwrap();
            stream.write_all(body).unwrap();
            stream.flush().unwrap();
        }
        compound.flush().unwrap();
    }
    buf
}

/// A Word document with a minimal FIB and contiguous 8-bit text, exercising
/// the `fcMin`/`fcMac` path, the paragraph/cell markers and the field logic.
fn doc_ole() -> Vec<u8> {
    let text = b"Heading\rBody text\rA\x07B\x07\rAfter\r";
    let mut word = vec![0u8; 1024];
    word[0..2].copy_from_slice(&0xA5ECu16.to_le_bytes()); // wIdent
    word[2..4].copy_from_slice(&193u16.to_le_bytes()); // nFib
    word[6..8].copy_from_slice(&0x0409u16.to_le_bytes()); // lid
    word[24..28].copy_from_slice(&1024u32.to_le_bytes()); // fcMin
    word[28..32].copy_from_slice(&(1024 + text.len() as u32).to_le_bytes()); // fcMac
    word.extend_from_slice(text);
    ole(&[("WordDocument", word), ("0Table", Vec::new())])
}

/// A deck with the full ordering machinery — persist directory, slide list,
/// a slide container and a notes container — so mutations reach the offset
/// arithmetic rather than bouncing off a missing structure.
fn ppt_ole() -> Vec<u8> {
    fn record(kind: u16, version: u16, instance: u16, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&((instance << 4) | version).to_le_bytes());
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(body);
        out
    }
    // Slide container: a title text header plus its text.
    let mut slide_body = record(0x0F9F, 0, 0, &0u32.to_le_bytes());
    slide_body.extend_from_slice(&record(0x0FA8, 0, 0, b"Slide title"));
    let slide = record(0x03EE, 0xF, 0, &slide_body);

    // Notes container: a notes-typed text header, plus the atom naming the
    // slide it annotates.
    let mut notes_body = record(0x03F1, 0, 0, &7u32.to_le_bytes());
    notes_body.extend_from_slice(&record(0x0F9F, 0, 0, &2u32.to_le_bytes()));
    notes_body.extend_from_slice(&record(0x0FA8, 0, 0, b"Speaker note"));
    let notes = record(0x03F0, 0xF, 0, &notes_body);

    // The slide's byte offset is only known once the preceding records are
    // sized, so the document container is built first and measured.
    let mut persist_body = Vec::new();
    persist_body.extend_from_slice(&(1u32 | (1u32 << 20)).to_le_bytes());
    persist_body.extend_from_slice(&0u32.to_le_bytes()); // patched below
    let persist = record(0x1772, 0, 0, &persist_body);

    let mut slide_persist = Vec::new();
    slide_persist.extend_from_slice(&1u32.to_le_bytes()); // persistIdRef
    slide_persist.extend_from_slice(&0u32.to_le_bytes()); // flags
    slide_persist.extend_from_slice(&1u32.to_le_bytes()); // cTexts
    slide_persist.extend_from_slice(&7u32.to_le_bytes()); // slideId
    let slide_list = record(0x0FF0, 0xF, 0, &record(0x03F3, 0, 0, &slide_persist));
    let document = record(0x03E8, 0xF, 0, &slide_list);

    let mut stream = Vec::new();
    stream.extend_from_slice(&persist);
    stream.extend_from_slice(&document);
    let slide_offset = stream.len() as u32;
    stream.extend_from_slice(&slide);
    stream.extend_from_slice(&notes);
    // Patch the persist directory now that the slide's offset is known.
    let offset_at = 8 + 4;
    stream[offset_at..offset_at + 4].copy_from_slice(&slide_offset.to_le_bytes());

    ole(&[("PowerPoint Document", stream)])
}

fn fixtures() -> Vec<(&'static str, &'static str, Vec<u8>)> {
    vec![
        ("docx", "m.docx", docx()),
        ("pptx", "m.pptx", pptx()),
        ("odt", "m.odt", odt()),
        ("ods", "m.ods", ods()),
        ("epub", "m.epub", epub()),
        (
            "rtf",
            "m.rtf",
            br"{\rtf1\ansi\ansicpg1252{\fonttbl{\f0 Arial;}}{\stylesheet{\s1\outlinelevel0 heading 1;}}\pard\b Bold\b0\par\pard\ls1\ilvl0 item\par\trowd A\cell B\cell\row{\footnote note}{\pict\pngblip 89504E47}}"
                .to_vec(),
        ),
        ("csv", "m.csv", b"a,b,c\n1,\"two\",3\n4,5,6\n".to_vec()),
        ("doc", "m.doc", doc_ole()),
        ("ppt", "m.ppt", ppt_ole()),
    ]
}

/// A mutated document must parse or fail cleanly — never panic, hang, or run
/// away. `parse_document_auto` is used (not a per-format entry point) so
/// format detection is fuzzed alongside the frontends.
#[test]
fn mutated_fixtures_never_panic_and_always_terminate() {
    let options = ParseOptions::default();
    let mut slowest = Duration::ZERO;
    let mut slowest_case = String::new();
    let mut parsed = 0u32;
    let mut rejected = 0u32;

    for (label, filename, seed) in fixtures() {
        // Seeded per fixture so one fixture's mutation sequence is stable
        // even if another's fixture bytes change.
        let mut rng = Lcg(0x5EED_0000 ^ label.bytes().map(u64::from).sum::<u64>());

        for index in 0..MUTATIONS_PER_FIXTURE {
            let mutated = mutate(&seed, &mut rng);
            let started = Instant::now();
            let outcome = parse_document_auto(&mutated, Some(filename), &options);
            let elapsed = started.elapsed();

            if elapsed > slowest {
                slowest = elapsed;
                slowest_case = format!("{label}#{index}");
            }
            assert!(
                elapsed < PER_CASE_BUDGET,
                "{label} mutation #{index} took {elapsed:?}, over the {PER_CASE_BUDGET:?} budget"
            );

            match outcome {
                Ok(document) => {
                    parsed += 1;
                    // A successful parse must still be internally consistent:
                    // a table's declared shape has to match its grid, or a
                    // downstream renderer indexes out of bounds.
                    assert_table_grids_are_consistent(&document, label, index);
                }
                // Every error must be one of the declared kinds. The match is
                // exhaustive on purpose: a new variant should force a
                // decision here rather than silently pass.
                Err(
                    DocumentError::UnsupportedFormat(_)
                    | DocumentError::Malformed { .. }
                    | DocumentError::Encrypted
                    | DocumentError::ResourceLimit { .. }
                    | DocumentError::MissingPart { .. }
                    | DocumentError::Io(_),
                ) => rejected += 1,
                Err(other) => {
                    panic!("{label} mutation #{index} produced an untyped error: {other}")
                }
            }
        }
    }

    // Both outcomes must actually occur, or the corpus is not exercising
    // anything: all-rejected would mean the mutations only ever break the
    // container, all-parsed that they never break anything.
    assert!(parsed > 0, "no mutated input parsed successfully");
    assert!(rejected > 0, "no mutated input was rejected");
    eprintln!(
        "mutation corpus: {parsed} parsed, {rejected} rejected, slowest {slowest:?} ({slowest_case})"
    );
}

fn assert_table_grids_are_consistent(
    document: &uparser_document_engine::CanonicalDocument,
    label: &str,
    index: u32,
) {
    use uparser_document_engine::{Block, CellSlot};

    fn check(blocks: &[Block], label: &str, index: u32) {
        for block in blocks {
            match block {
                Block::Table { table } => {
                    assert_eq!(
                        table.grid.len(),
                        table.rows,
                        "{label} mutation #{index}: table rows disagree with grid height"
                    );
                    for row in &table.grid {
                        assert_eq!(
                            row.len(),
                            table.columns,
                            "{label} mutation #{index}: ragged table grid row"
                        );
                        for slot in row {
                            if let CellSlot::Covered {
                                origin_row,
                                origin_column,
                            } = slot
                            {
                                assert!(
                                    *origin_row < table.rows && *origin_column < table.columns,
                                    "{label} mutation #{index}: covered cell points outside the grid"
                                );
                            }
                        }
                    }
                    for row in &table.grid {
                        for slot in row {
                            if let CellSlot::Origin(cell) = slot {
                                check(&cell.blocks, label, index);
                            }
                        }
                    }
                }
                Block::List { list } => {
                    for item in &list.items {
                        check(&item.blocks, label, index);
                    }
                }
                Block::BlockQuote { blocks } => check(blocks, label, index),
                _ => {}
            }
        }
    }

    for unit in &document.units {
        check(&unit.blocks, label, index);
    }
    for note in &document.notes {
        check(&note.blocks, label, index);
    }
}

/// Rendering is as exposed to malformed input as parsing: it walks the same
/// structures. A document that parses must also render without panicking.
#[test]
fn rendering_a_mutated_document_never_panics() {
    let options = ParseOptions::default();
    for (label, filename, seed) in fixtures() {
        let mut rng = Lcg(0xD0C_0000 ^ label.bytes().map(u64::from).sum::<u64>());
        for index in 0..MUTATIONS_PER_FIXTURE {
            let mutated = mutate(&seed, &mut rng);
            let Ok(document) = parse_document_auto(&mutated, Some(filename), &options) else {
                continue;
            };
            let markdown = uparser_document_engine::render::markdown(&document);
            // Renderer output is always valid UTF-8 by construction; the real
            // assertion is that neither call panicked.
            assert!(
                markdown.is_char_boundary(markdown.len()),
                "{label} mutation #{index}"
            );
            uparser_document_engine::render::document_json(&document)
                .unwrap_or_else(|e| panic!("{label} mutation #{index} failed to serialize: {e}"));
        }
    }
}

/// The XML parts of a package, so a mutation can corrupt *content* while
/// leaving a perfectly valid container around it.
type XmlPackage = (&'static str, &'static str, Vec<(&'static str, Vec<u8>)>);

fn xml_packages() -> Vec<XmlPackage> {
    let docx_parts = vec![
        (
            "[Content_Types].xml",
            b"<Types><Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/></Types>".to_vec(),
        ),
        (
            "_rels/.rels",
            b"<Relationships><Relationship Id=\"r0\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>".to_vec(),
        ),
        (
            "word/styles.xml",
            b"<w:styles xmlns:w=\"w\"><w:style w:type=\"character\" w:styleId=\"C\"><w:rPr><w:b/></w:rPr></w:style></w:styles>".to_vec(),
        ),
        (
            "word/document.xml",
            b"<w:document xmlns:w=\"w\"><w:body><w:p><w:r><w:rPr><w:rStyle w:val=\"C\"/></w:rPr><w:t>x</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:tcPr><w:gridSpan w:val=\"3\"/><w:vMerge w:val=\"restart\"/></w:tcPr><w:p><w:r><w:t>c</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:tcPr><w:vMerge/></w:tcPr><w:p/></w:tc></w:tr></w:tbl></w:body></w:document>".to_vec(),
        ),
    ];
    let odt_parts = vec![
        (
            "mimetype",
            b"application/vnd.oasis.opendocument.text".to_vec(),
        ),
        (
            "content.xml",
            b"<office:document-content xmlns:office=\"office\" xmlns:text=\"text\" xmlns:table=\"table\"><office:body><office:text><text:list><text:list-item><text:p>a</text:p><text:list><text:list-item><text:p>b</text:p></text:list-item></text:list></text:list-item></text:list><table:table><table:table-row table:number-rows-repeated=\"4\"><table:table-cell table:number-columns-spanned=\"3\" table:number-rows-spanned=\"2\"><text:p>c</text:p></table:table-cell></table:table-row></table:table></office:text></office:body></office:document-content>".to_vec(),
        ),
    ];
    let epub_parts = vec![
        ("mimetype", b"application/epub+zip".to_vec()),
        (
            "META-INF/container.xml",
            b"<container><rootfiles><rootfile full-path=\"b.opf\" media-type=\"application/oebps-package+xml\"/></rootfiles></container>".to_vec(),
        ),
        (
            "b.opf",
            b"<package xmlns:dc=\"dc\"><metadata><dc:title>T</dc:title></metadata><manifest><item id=\"c\" href=\"c.xhtml\" media-type=\"application/xhtml+xml\"/></manifest><spine><itemref idref=\"c\"/></spine></package>".to_vec(),
        ),
        (
            "c.xhtml",
            b"<html xmlns=\"http://www.w3.org/1999/xhtml\"><body><table><tr><th colspan=\"2\">A</th></tr><tr><td rowspan=\"3\">B</td><td>C</td></tr></table><ul><li>x<ol><li>y</li></ol></li></ul></body></html>".to_vec(),
        ),
    ];
    vec![
        ("docx-parts", "p.docx", docx_parts),
        ("odt-parts", "p.odt", odt_parts),
        ("epub-parts", "p.epub", epub_parts),
    ]
}

/// Corrupting a package's *contents* rather than its bytes keeps the ZIP
/// valid, so every mutation reaches the XML frontends instead of dying at the
/// container. This is where span arithmetic, style cascades and list nesting
/// actually get exercised against hostile input.
#[test]
fn mutated_xml_parts_inside_a_valid_container_never_panic() {
    let options = ParseOptions::default();
    let mut parsed = 0u32;
    let mut rejected = 0u32;

    for (label, filename, parts) in xml_packages() {
        let mut rng = Lcg(0xBAD_0000 ^ label.bytes().map(u64::from).sum::<u64>());
        for index in 0..MUTATIONS_PER_FIXTURE {
            // Mutate exactly one part per iteration; the rest stay valid so
            // the parser gets far enough in to reach the damaged one.
            let victim = rng.below(parts.len());
            let mutated: Vec<(&str, Vec<u8>)> = parts
                .iter()
                .enumerate()
                .map(|(position, (name, body))| {
                    let body = if position == victim {
                        mutate(body, &mut rng)
                    } else {
                        body.clone()
                    };
                    (*name, body)
                })
                .collect();
            let borrowed: Vec<(&str, &[u8])> = mutated
                .iter()
                .map(|(name, body)| (*name, body.as_slice()))
                .collect();
            let bytes = zip_fixture(&borrowed);

            let started = Instant::now();
            let outcome = parse_document_auto(&bytes, Some(filename), &options);
            assert!(
                started.elapsed() < PER_CASE_BUDGET,
                "{label} mutation #{index} exceeded the time budget"
            );

            match outcome {
                Ok(document) => {
                    parsed += 1;
                    assert_table_grids_are_consistent(&document, label, index);
                    // Rendering walks the same structures, so it is part of
                    // what "did not panic" has to cover.
                    let _ = uparser_document_engine::render::markdown(&document);
                }
                Err(
                    DocumentError::UnsupportedFormat(_)
                    | DocumentError::Malformed { .. }
                    | DocumentError::Encrypted
                    | DocumentError::ResourceLimit { .. }
                    | DocumentError::MissingPart { .. }
                    | DocumentError::Io(_),
                ) => rejected += 1,
                Err(other) => {
                    panic!("{label} mutation #{index} produced an untyped error: {other}")
                }
            }
        }
    }
    assert!(
        parsed > 0 && rejected > 0,
        "{parsed} parsed, {rejected} rejected"
    );
    eprintln!("xml-part mutation corpus: {parsed} parsed, {rejected} rejected");
}

/// The same input must always produce the same output. Anything that varies
/// (hash iteration order leaking into output, an uninitialised read) shows up
/// here as a diff.
#[test]
fn parsing_is_deterministic() {
    let options = ParseOptions::default();
    for (label, filename, seed) in fixtures() {
        let mut rng = Lcg(0xDEED_0000 ^ label.bytes().map(u64::from).sum::<u64>());
        for index in 0..50 {
            let mutated = mutate(&seed, &mut rng);
            let first = parse_document_auto(&mutated, Some(filename), &options)
                .map(|d| uparser_document_engine::render::markdown(&d));
            let second = parse_document_auto(&mutated, Some(filename), &options)
                .map(|d| uparser_document_engine::render::markdown(&d));
            match (first, second) {
                (Ok(a), Ok(b)) => {
                    assert_eq!(a, b, "{label} mutation #{index} rendered differently twice")
                }
                (Err(a), Err(b)) => assert_eq!(
                    a.to_string(),
                    b.to_string(),
                    "{label} mutation #{index} failed differently twice"
                ),
                _ => panic!("{label} mutation #{index} succeeded once and failed once"),
            }
        }
    }
}
