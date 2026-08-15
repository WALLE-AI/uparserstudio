//! Local structured-document parsing for uparser.
//!
//! Format frontends recover source semantics into one canonical document.
//! Renderers and compatibility adapters consume that model; frontends never
//! construct Markdown directly.

mod detect;
mod error;
mod formats;
mod model;
mod ooxml;
mod options;
mod package;
pub mod render;

pub use detect::{DocumentFormat, detect_format};
pub use error::{DocumentError, ParseWarning, WarningCode};
pub use model::{
    AnchorId, Asset, AssetId, Block, CanonicalDocument, Cell, CellSlot, CellValueKind,
    DocumentMetadata, DocumentUnit, FormulaSource, ImageSource, Inline, LinkTarget, List, ListItem,
    ListMarker, Note, NoteKind, Style, Table, TableKind, UnitKind,
};
pub use options::{ParseOptions, ResourceLimits};

/// Parse bytes into the canonical structured-document model.
pub fn parse_document(
    bytes: &[u8],
    format: DocumentFormat,
    options: &ParseOptions,
) -> Result<CanonicalDocument, DocumentError> {
    if bytes.len() as u64 > options.limits.max_input_bytes {
        return Err(DocumentError::ResourceLimit {
            limit: "max_input_bytes",
            detail: format!(
                "input is {} bytes, limit is {}",
                bytes.len(),
                options.limits.max_input_bytes
            ),
        });
    }
    formats::parse(bytes, format, options)
}

/// Detect and parse bytes. The filename is only a hint for formats without
/// a reliable signature, such as CSV and TSV.
pub fn parse_document_auto(
    bytes: &[u8],
    filename_hint: Option<&str>,
    options: &ParseOptions,
) -> Result<CanonicalDocument, DocumentError> {
    let format = detect_format(bytes, filename_hint);
    if format == DocumentFormat::Unknown {
        return Err(DocumentError::UnsupportedFormat(format));
    }
    parse_document(bytes, format, options)
}

/// Parse a document and render it with the shared Markdown renderer.
pub fn parse_to_markdown(
    bytes: &[u8],
    filename_hint: Option<&str>,
    options: &ParseOptions,
) -> Result<String, DocumentError> {
    let document = parse_document_auto(bytes, filename_hint, options)?;
    Ok(render::markdown(&document))
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use std::io::Write;

    fn package(parts: &[(&str, &str)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for (name, body) in parts {
                writer.start_file(name, options).unwrap();
                writer.write_all(body.as_bytes()).unwrap();
            }
            writer.finish().unwrap();
        }
        bytes
    }

    #[test]
    fn parses_docx_headings_lists_and_tables() {
        let bytes = package(&[
            (
                "[Content_Types].xml",
                "<Types><Override ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/></Types>",
            ),
            (
                "word/document.xml",
                r#"<w:document xmlns:w="w"><w:body>
                <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Report</w:t></w:r></w:p>
                <w:p><w:r><w:t>Introduction</w:t></w:r></w:p>
                <w:p><w:pPr><w:numPr/></w:pPr><w:r><w:t>First item</w:t></w:r></w:p>
                <w:tbl><w:tr><w:tc><w:p><w:r><w:t>Name</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Value</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
            </w:body></w:document>"#,
            ),
        ]);
        let document =
            parse_document_auto(&bytes, Some("report.docx"), &ParseOptions::default()).unwrap();
        assert_eq!(document.metadata.format, DocumentFormat::Docx);
        assert!(matches!(
            document.units[0].blocks[0],
            Block::Heading { level: 1, .. }
        ));
        assert!(
            document.units[0]
                .blocks
                .iter()
                .any(|block| matches!(block, Block::List { .. }))
        );
        assert!(
            document.units[0]
                .blocks
                .iter()
                .any(|block| matches!(block, Block::Table { .. }))
        );
    }

    #[test]
    fn parses_docx_relationships_notes_styles_numbering_assets_and_spans() {
        let bytes = package(&[
            (
                "[Content_Types].xml",
                "<Types><Override ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/></Types>",
            ),
            (
                "word/_rels/document.xml.rels",
                r#"<Relationships>
                <Relationship Id="rLink" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.com" TargetMode="External"/>
                <Relationship Id="rImage" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/>
            </Relationships>"#,
            ),
            (
                "word/styles.xml",
                r#"<w:styles xmlns:w="w">
                <w:style w:type="paragraph" w:styleId="BaseHeading"><w:name w:val="Heading 2"/></w:style>
                <w:style w:type="paragraph" w:styleId="CustomHeading"><w:basedOn w:val="BaseHeading"/></w:style>
            </w:styles>"#,
            ),
            (
                "word/numbering.xml",
                r#"<w:numbering xmlns:w="w">
                <w:abstractNum w:abstractNumId="9"><w:lvl w:ilvl="0"><w:start w:val="3"/><w:numFmt w:val="decimal"/></w:lvl></w:abstractNum>
                <w:num w:numId="12"><w:abstractNumId w:val="9"/></w:num>
            </w:numbering>"#,
            ),
            (
                "word/footnotes.xml",
                r#"<w:footnotes xmlns:w="w"><w:footnote w:id="2"><w:p><w:r><w:t>Footnote body</w:t></w:r></w:p></w:footnote></w:footnotes>"#,
            ),
            ("word/media/image1.png", "not-a-real-png-but-stable-bytes"),
            (
                "word/document.xml",
                r#"<w:document xmlns:w="w" xmlns:r="r" xmlns:a="a"><w:body>
                <w:p><w:pPr><w:pStyle w:val="CustomHeading"/></w:pPr><w:r><w:rPr><w:b/></w:rPr><w:t>Styled heading</w:t></w:r></w:p>
                <w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="12"/></w:numPr></w:pPr><w:hyperlink r:id="rLink"><w:r><w:t>linked item</w:t></w:r></w:hyperlink><w:r><w:footnoteReference w:id="2"/><a:blip r:embed="rImage"/></w:r></w:p>
                <w:tbl>
                  <w:tr><w:trPr><w:tblHeader/></w:trPr><w:tc><w:tcPr><w:gridSpan w:val="2"/><w:vMerge w:val="restart"/></w:tcPr><w:p><w:r><w:t>Merged</w:t></w:r></w:p></w:tc></w:tr>
                  <w:tr><w:tc><w:tcPr><w:gridSpan w:val="2"/><w:vMerge/></w:tcPr><w:p/></w:tc></w:tr>
                </w:tbl>
            </w:body></w:document>"#,
            ),
        ]);
        let document =
            parse_document_auto(&bytes, Some("rich.docx"), &ParseOptions::default()).unwrap();
        let Block::Heading { level, content } = &document.units[0].blocks[0] else {
            panic!()
        };
        assert_eq!(*level, 2);
        assert!(matches!(&content[0], Inline::Text { style, .. } if style.bold));
        let Block::List { list } = &document.units[0].blocks[1] else {
            panic!()
        };
        assert_eq!(list.marker, ListMarker::Decimal);
        assert_eq!(list.start, Some(3));
        assert_eq!(document.notes[0].id, "footnote-2");
        assert_eq!(document.assets.len(), 1);
        assert_eq!(document.assets[0].media_type, "image/png");
        let Block::Table { table } = &document.units[0].blocks[2] else {
            panic!()
        };
        assert_eq!((table.rows, table.columns, table.header_rows), (2, 2, 1));
        let CellSlot::Origin(cell) = &table.grid[0][0] else {
            panic!()
        };
        assert_eq!((cell.row_span, cell.column_span), (2, 2));
        let markdown = render::markdown(&document);
        assert!(markdown.contains("[linked item](https://example.com)"));
        assert!(markdown.contains("[^footnote-2]"));
        assert!(markdown.contains("asset-"));
        assert!(markdown.contains("rowspan=\"2\" colspan=\"2\""));
    }

    #[test]
    fn parses_pptx_slides_and_speaker_notes() {
        let bytes = package(&[
            (
                "[Content_Types].xml",
                "<Types><Override ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml\"/></Types>",
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree>
                <p:sp><p:nvSpPr><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>Quarterly Review</a:t></a:r></a:p></p:txBody></p:sp>
                <p:sp><p:txBody><a:p><a:pPr lvl="0"/><a:r><a:t>Revenue grew</a:t></a:r></a:p></p:txBody></p:sp>
            </p:spTree></p:cSld></p:sld>"#,
            ),
            (
                "ppt/notesSlides/notesSlide1.xml",
                r#"<p:notes xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:txBody><a:p><a:r><a:t>Explain the chart</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:notes>"#,
            ),
        ]);
        let document =
            parse_document_auto(&bytes, Some("review.pptx"), &ParseOptions::default()).unwrap();
        assert_eq!(document.units.len(), 1);
        assert_eq!(document.units[0].label.as_deref(), Some("Quarterly Review"));
        assert!(matches!(document.units[0].blocks[1], Block::List { .. }));
        assert_eq!(document.notes.len(), 1);
    }

    #[test]
    fn parses_pptx_relationship_order_tables_images_and_notes() {
        let bytes = package(&[
            (
                "[Content_Types].xml",
                "<Types><Override ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml\"/></Types>",
            ),
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId r:id="rSecond"/><p:sldId r:id="rFirst"/></p:sldIdLst></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships>
                <Relationship Id="rFirst" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/>
                <Relationship Id="rSecond" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide2.xml"/>
            </Relationships>"#,
            ),
            (
                "ppt/slides/_rels/slide2.xml.rels",
                r#"<Relationships>
                <Relationship Id="rImage" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/>
                <Relationship Id="rNotes" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/notesSlide" Target="../notesSlides/notesSlide2.xml"/>
            </Relationships>"#,
            ),
            ("ppt/media/image1.png", "ppt-image-bytes"),
            (
                "ppt/slides/slide2.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:r="r"><p:cSld><p:spTree>
                <p:sp><p:nvSpPr><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>Second is first</a:t></a:r></a:p></p:txBody></p:sp>
                <p:graphicFrame><a:graphic><a:graphicData><a:tbl>
                  <a:tr><a:tc gridSpan="2" rowSpan="2"><a:txBody><a:p><a:r><a:t>Merged cell</a:t></a:r></a:p></a:txBody></a:tc><a:tc hMerge="1"><a:txBody><a:p/></a:txBody></a:tc></a:tr>
                  <a:tr><a:tc vMerge="1"><a:txBody><a:p/></a:txBody></a:tc><a:tc hMerge="1" vMerge="1"><a:txBody><a:p/></a:txBody></a:tc></a:tr>
                </a:tbl></a:graphicData></a:graphic></p:graphicFrame>
                <p:pic><p:nvPicPr><p:cNvPr name="Chart" descr="Quarterly chart"/></p:nvPicPr><p:blipFill><a:blip r:embed="rImage"/></p:blipFill></p:pic>
            </p:spTree></p:cSld></p:sld>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>First is second</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#,
            ),
            (
                "ppt/notesSlides/notesSlide2.xml",
                r#"<p:notes xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:txBody><a:p><a:r><a:t>Relationship note</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:notes>"#,
            ),
        ]);
        let document =
            parse_document_auto(&bytes, Some("deck.pptx"), &ParseOptions::default()).unwrap();
        assert_eq!(document.units[0].label.as_deref(), Some("Second is first"));
        assert_eq!(document.units[1].label.as_deref(), Some("First is second"));
        let Block::Table { table } = &document.units[0].blocks[1] else {
            panic!()
        };
        let CellSlot::Origin(cell) = &table.grid[0][0] else {
            panic!()
        };
        assert_eq!((cell.row_span, cell.column_span), (2, 2));
        assert!(
            matches!(&document.units[0].blocks[2], Block::Figure { alt: Some(value), .. } if value == "Quarterly chart")
        );
        assert_eq!(document.assets.len(), 1);
        assert_eq!(
            document.notes[0].blocks[0],
            Block::paragraph("Relationship note")
        );
    }

    #[test]
    fn package_and_xml_resource_limits_return_typed_errors() {
        let bytes = package(&[
            (
                "[Content_Types].xml",
                "<Types><Override ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/></Types>",
            ),
            (
                "word/document.xml",
                "<w:document xmlns:w=\"w\"><w:body><w:p><w:r><w:t>text</w:t></w:r></w:p></w:body></w:document>",
            ),
        ]);
        let mut package_limited = ParseOptions::default();
        package_limited.limits.max_total_uncompressed_bytes = 1;
        assert!(matches!(
            parse_document_auto(&bytes, Some("limited.docx"), &package_limited),
            Err(DocumentError::ResourceLimit {
                limit: "max_total_uncompressed_bytes",
                ..
            })
        ));

        let mut xml_limited = ParseOptions::default();
        xml_limited.limits.max_xml_nodes = 2;
        assert!(matches!(
            parse_document_auto(&bytes, Some("limited.docx"), &xml_limited),
            Err(DocumentError::ResourceLimit {
                limit: "max_xml_nodes",
                ..
            })
        ));
    }

    #[test]
    fn missing_required_ooxml_part_is_a_typed_error() {
        let bytes = package(&[(
            "[Content_Types].xml",
            "<Types><Override ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/></Types>",
        )]);
        assert!(matches!(
            parse_document_auto(&bytes, Some("broken.docx"), &ParseOptions::default()),
            Err(DocumentError::MissingPart { part }) if part == "word/document.xml"
        ));
    }

    #[test]
    fn parses_odt_flow_lists_links_images_and_spanned_tables() {
        let bytes = package(&[
            ("mimetype", "application/vnd.oasis.opendocument.text"),
            ("Pictures/cover.png", "stable-odf-image"),
            (
                "meta.xml",
                r#"<office:document-meta xmlns:office="office" xmlns:dc="dc" xmlns:meta="meta"><office:meta><dc:title>ODF metadata title</dc:title><dc:creator>Writer</dc:creator><dc:language>zh-CN</dc:language><meta:creation-date>2026-01-02T03:04:05</meta:creation-date><meta:generator>uparser-test</meta:generator></office:meta></office:document-meta>"#,
            ),
            (
                "styles.xml",
                r#"<office:document-styles xmlns:office="office" xmlns:text="text" xmlns:style="style"><office:styles><text:list-style style:name="LNum"><text:list-level-style-number text:level="1"/></text:list-style></office:styles></office:document-styles>"#,
            ),
            (
                "content.xml",
                r##"<office:document-content xmlns:office="office" xmlns:text="text" xmlns:table="table" xmlns:draw="draw" xmlns:xlink="xlink"><office:body><office:text>
                <text:h text:outline-level="2">ODF report</text:h>
                <text:p>Visit <text:a xlink:href="https://example.com">example</text:a><text:line-break/>today<text:note text:id="note-1" text:note-class="footnote"><text:note-citation>1</text:note-citation><text:note-body><text:p>ODF note body</text:p></text:note-body></text:note></text:p>
                <text:list text:style-name="LNum"><text:list-item><text:p>First</text:p><text:list><text:list-item><text:p>Nested</text:p></text:list-item></text:list></text:list-item></text:list>
                <text:p><draw:image xlink:href="Pictures/cover.png"/></text:p>
                <table:table table:name="Metrics"><table:table-row>
                  <table:table-cell table:number-columns-spanned="2" table:number-rows-spanned="2"><text:p>Merged</text:p></table:table-cell><table:covered-table-cell/>
                </table:table-row><table:table-row table:number-rows-repeated="1"><table:covered-table-cell/><table:covered-table-cell/></table:table-row></table:table>
                </office:text></office:body></office:document-content>"##,
            ),
        ]);
        let document =
            parse_document_auto(&bytes, Some("report.odt"), &ParseOptions::default()).unwrap();
        assert_eq!(document.metadata.format, DocumentFormat::Odt);
        assert_eq!(
            document.metadata.title.as_deref(),
            Some("ODF metadata title")
        );
        assert_eq!(document.metadata.author.as_deref(), Some("Writer"));
        assert_eq!(document.metadata.language.as_deref(), Some("zh-CN"));
        assert_eq!(document.units.len(), 1);
        assert!(matches!(
            document.units[0].blocks[0],
            Block::Heading { level: 2, .. }
        ));
        assert!(matches!(
            document.units[0].blocks[2],
            Block::List {
                list: List {
                    marker: ListMarker::Decimal,
                    ..
                }
            }
        ));
        assert_eq!(document.notes[0].id, "note-1");
        assert_eq!(
            document.notes[0].blocks[0],
            Block::paragraph("ODF note body")
        );
        assert_eq!(document.assets.len(), 1);
        let Block::Table { table } = document.units[0].blocks.last().unwrap() else {
            panic!()
        };
        let CellSlot::Origin(cell) = &table.grid[0][0] else {
            panic!()
        };
        assert_eq!((cell.row_span, cell.column_span), (2, 2));
        let markdown = render::markdown(&document);
        assert!(markdown.contains("[example](https://example.com)"));
        assert!(markdown.contains("asset-"));
    }

    #[test]
    fn parses_odp_pages_as_ordered_slide_units() {
        let bytes = package(&[
            (
                "mimetype",
                "application/vnd.oasis.opendocument.presentation",
            ),
            (
                "content.xml",
                r#"<office:document-content xmlns:office="office" xmlns:draw="draw" xmlns:text="text"><office:body><office:presentation>
                <draw:page draw:name="Opening"><draw:frame><draw:text-box><text:h text:outline-level="1">Title</text:h><text:p>Body</text:p></draw:text-box></draw:frame></draw:page>
                <draw:page draw:name="Details"><draw:frame><draw:text-box><text:p>Second slide</text:p></draw:text-box></draw:frame></draw:page>
                </office:presentation></office:body></office:document-content>"#,
            ),
        ]);
        let document =
            parse_document_auto(&bytes, Some("deck.odp"), &ParseOptions::default()).unwrap();
        assert_eq!(document.units.len(), 2);
        assert_eq!(document.units[0].kind, UnitKind::Slide);
        assert_eq!(document.units[0].label.as_deref(), Some("Opening"));
        assert_eq!(document.units[1].label.as_deref(), Some("Details"));
        assert!(matches!(document.units[0].blocks[0], Block::Heading { .. }));
    }

    #[test]
    fn rejects_encrypted_and_overexpanded_odf_packages() {
        let encrypted = package(&[
            ("mimetype", "application/vnd.oasis.opendocument.text"),
            ("content.xml", "<document/>"),
            (
                "META-INF/manifest.xml",
                "<manifest><encryption-data/></manifest>",
            ),
        ]);
        assert!(matches!(
            parse_document_auto(&encrypted, Some("secret.odt"), &ParseOptions::default()),
            Err(DocumentError::Encrypted)
        ));

        let expanded = package(&[
            ("mimetype", "application/vnd.oasis.opendocument.text"),
            (
                "content.xml",
                r#"<document xmlns:table="table"><table:table><table:table-row table:number-rows-repeated="100"><table:table-cell/></table:table-row></table:table></document>"#,
            ),
        ]);
        let mut options = ParseOptions::default();
        options.limits.max_expansion = 10;
        assert!(matches!(
            parse_document_auto(&expanded, Some("large.odt"), &options),
            Err(DocumentError::ResourceLimit {
                limit: "max_expansion",
                ..
            })
        ));
    }

    #[test]
    fn parses_epub_metadata_spine_xhtml_assets_lists_and_tables() {
        let bytes = package(&[
            ("mimetype", "application/epub+zip"),
            (
                "META-INF/container.xml",
                r#"<container><rootfiles><rootfile full-path="OPS/book.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
            ),
            (
                "OPS/book.opf",
                r#"<package xmlns:dc="dc"><metadata><dc:title>Example book</dc:title><dc:creator>A. Writer</dc:creator><dc:language>zh-CN</dc:language></metadata><manifest>
                <item id="first" href="text/first.xhtml" media-type="application/xhtml+xml"/>
                <item id="second" href="text/second.xhtml" media-type="application/xhtml+xml"/>
                <item id="cover" href="images/cover.png" media-type="image/png"/>
                </manifest><spine><itemref idref="second"/><itemref idref="first"/></spine></package>"#,
            ),
            ("OPS/images/cover.png", "epub-image-bytes"),
            (
                "OPS/text/first.xhtml",
                r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><h1>First file</h1><p>Last chapter</p></body></html>"#,
            ),
            (
                "OPS/text/second.xhtml",
                r##"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><head><style>ignored</style></head><body>
                <h2 id="start">Spine first</h2><p><strong>Bold</strong> and <a href="first.xhtml#next">linked</a><br/><img src="../images/cover.png" alt="Cover"/></p>
                <p>Equation <math><mi>x</mi><mo>+</mo><mn>1</mn></math><a epub:type="noteref" href="#fn1">1</a></p>
                <aside epub:type="footnote" id="fn1"><p>EPUB footnote body</p></aside>
                <ol><li><p>One</p><ul><li><p>Nested</p></li></ul></li></ol>
                <table><tr><th colspan="2">Header</th></tr><tr><td rowspan="2">A</td><td>B</td></tr><tr><td>C</td></tr></table>
                <blockquote><p>Quoted</p></blockquote><pre>let x = 1;</pre>
                </body></html>"##,
            ),
        ]);
        let document =
            parse_document_auto(&bytes, Some("book.epub"), &ParseOptions::default()).unwrap();
        assert_eq!(document.metadata.title.as_deref(), Some("Example book"));
        assert_eq!(document.metadata.author.as_deref(), Some("A. Writer"));
        assert_eq!(document.metadata.language.as_deref(), Some("zh-CN"));
        assert_eq!(document.units.len(), 2);
        assert_eq!(document.units[0].kind, UnitKind::Chapter);
        assert_eq!(document.units[0].label.as_deref(), Some("Spine first"));
        assert_eq!(document.units[1].label.as_deref(), Some("First file"));
        assert_eq!(document.assets.len(), 1);
        assert_eq!(document.notes.len(), 1);
        assert_eq!(document.notes[0].id, "OPS/text/second.xhtml#fn1");
        assert_eq!(
            document.notes[0].blocks[0],
            Block::paragraph("EPUB footnote body")
        );
        let table = document.units[0]
            .blocks
            .iter()
            .find_map(|block| match block {
                Block::Table { table } => Some(table),
                _ => None,
            })
            .unwrap();
        assert_eq!((table.rows, table.columns, table.header_rows), (3, 2, 1));
        let CellSlot::Origin(cell) = &table.grid[1][0] else {
            panic!()
        };
        assert_eq!(cell.row_span, 2);
        let markdown = render::markdown(&document);
        assert!(markdown.contains("**Bold**"));
        assert!(markdown.contains("[linked](first.xhtml#next)"));
        assert!(markdown.contains("asset-"));
        assert!(markdown.contains("[^OPS/text/second.xhtml#fn1]"));
        assert!(markdown.contains("x+1"));
        assert!(markdown.contains("> Quoted"));
        assert!(markdown.contains("let x = 1;"));
    }

    #[test]
    fn missing_epub_rootfile_is_a_typed_error() {
        let bytes = package(&[
            ("mimetype", "application/epub+zip"),
            (
                "META-INF/container.xml",
                "<container><rootfiles/></container>",
            ),
        ]);
        assert!(matches!(
            parse_document_auto(&bytes, Some("broken.epub"), &ParseOptions::default()),
            Err(DocumentError::MissingPart { part }) if part == "EPUB rootfile declaration"
        ));
    }

    #[test]
    fn parses_rtf_unicode_styles_fields_notes_lists_tables_bookmarks_and_pictures() {
        let bytes = br#"{\rtf1\ansi\ansicpg1252
        {\fonttbl{\f0 Arial;}}
        \pard\outlinelevel1\b Heading\b0\par
        \pard Plain \i italic\i0 \u55357?\u56832?\par
        {\field{\*\fldinst HYPERLINK "https://example.com"}{\fldrslt Link}}\par
        {\*\bkmkstart target}Target text\par
        {\footnote Note body}\par
        \pard\ls1\ilvl0 First\par\pard\ls1\ilvl0 Second\par
        \pard\trowd A\cell B\cell\row
        \pard After table\par
        {\pict\pngblip 89504E47}
        }"#;
        let document =
            parse_document_auto(bytes, Some("rich.rtf"), &ParseOptions::default()).unwrap();
        assert_eq!(document.metadata.format, DocumentFormat::Rtf);
        assert!(matches!(
            document.units[0].blocks[0],
            Block::Heading { level: 2, .. }
        ));
        let markdown = render::markdown(&document);
        assert!(markdown.contains("😀"));
        assert!(markdown.contains("*italic*"));
        assert!(
            markdown.contains("[Link](https://example.com)"),
            "{markdown}"
        );
        assert!(markdown.contains("[^rtf-note-1]"));
        assert!(markdown.contains("First"));
        assert!(markdown.contains("| A | B |"));
        assert!(markdown.find("| A | B |").unwrap() < markdown.find("After table").unwrap());
        assert_eq!(document.notes.len(), 1);
        assert_eq!(document.notes[0].blocks[0], Block::paragraph("Note body"));
        assert_eq!(document.assets.len(), 1);
        assert_eq!(document.assets[0].media_type, "image/png");
    }

    #[test]
    fn parses_rtf_shift_jis_and_cyrillic_hex_runs() {
        let japanese = br#"{\rtf1\ansi\ansicpg932 \'82\'a0\par}"#;
        let document =
            parse_document_auto(japanese, Some("jp.rtf"), &ParseOptions::default()).unwrap();
        assert!(render::markdown(&document).contains('あ'));

        let cyrillic = br#"{\rtf1\ansi\ansicpg1251 \'cf\'f0\'e8\'e2\'e5\'f2\par}"#;
        let document =
            parse_document_auto(cyrillic, Some("ru.rtf"), &ParseOptions::default()).unwrap();
        let markdown = render::markdown(&document);
        assert!(markdown.contains("Привет"), "{markdown}");
    }

    #[test]
    fn rtf_depth_and_token_budgets_are_typed_errors() {
        let mut depth_options = ParseOptions::default();
        depth_options.limits.max_xml_depth = 2;
        assert!(matches!(
            parse_document_auto(br#"{\rtf1{{{deep}}}}"#, Some("deep.rtf"), &depth_options),
            Err(DocumentError::ResourceLimit {
                limit: "max_xml_depth",
                ..
            })
        ));

        let mut token_options = ParseOptions::default();
        token_options.limits.max_xml_nodes = 2;
        assert!(matches!(
            parse_document_auto(br#"{\rtf1 text}"#, Some("many.rtf"), &token_options),
            Err(DocumentError::ResourceLimit {
                limit: "max_xml_nodes",
                ..
            })
        ));
    }
}
