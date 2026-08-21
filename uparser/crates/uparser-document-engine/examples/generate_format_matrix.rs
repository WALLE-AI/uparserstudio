use image::{ImageBuffer, ImageFormat, Rgb};
use std::fs;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

fn zip_fixture(parts: &[(&str, &[u8])]) -> Vec<u8> {
    let mut output = Vec::new();
    {
        let mut writer = zip::ZipWriter::new(Cursor::new(&mut output));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, body) in parts {
            writer.start_file(*name, options).unwrap();
            writer.write_all(body).unwrap();
        }
        writer.finish().unwrap();
    }
    output
}

fn ole(streams: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut output = Vec::new();
    {
        let mut compound = cfb::CompoundFile::create(Cursor::new(&mut output)).unwrap();
        for (name, body) in streams {
            let mut stream = compound.create_stream(name).unwrap();
            stream.write_all(body).unwrap();
        }
        compound.flush().unwrap();
    }
    output
}

fn pdf() -> Vec<u8> {
    let content = concat!(
        "BT /F1 18 Tf 72 720 Td (Format Matrix PDF) Tj ",
        "0 -30 Td /F1 11 Tf (This is a born digital document with a searchable text layer.) Tj ",
        "0 -18 Td (It contains enough native text to satisfy the reliability profiler.) Tj ",
        "0 -18 Td (The matrix validates format detection analysis routing and parsing.) Tj ",
        "0 -18 Td (Native extraction must preserve these words without rasterization.) Tj ",
        "0 -18 Td (Name Value alpha forty-two beta seven.) Tj ET"
    );
    let objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{}\nendstream", content.len(), content),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
    ];
    let mut output = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(output.len());
        write!(output, "{} 0 obj\n{}\nendobj\n", index + 1, object).unwrap();
    }
    let xref = output.len();
    write!(
        output,
        "xref\n0 {}\n0000000000 65535 f \n",
        objects.len() + 1
    )
    .unwrap();
    for offset in offsets {
        writeln!(output, "{offset:010} 00000 n ").unwrap();
    }
    write!(
        output,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    )
    .unwrap();
    output
}

fn doc() -> Vec<u8> {
    let text = b"Format Matrix DOC\rLegacy Word body\rA\x07B\x07\r";
    let mut word = vec![0u8; 1024];
    word[0..2].copy_from_slice(&0xA5ECu16.to_le_bytes());
    word[2..4].copy_from_slice(&193u16.to_le_bytes());
    word[6..8].copy_from_slice(&0x0409u16.to_le_bytes());
    word[24..28].copy_from_slice(&1024u32.to_le_bytes());
    word[28..32].copy_from_slice(&(1024 + text.len() as u32).to_le_bytes());
    word.extend_from_slice(text);
    ole(&[("WordDocument", word), ("0Table", Vec::new())])
}

fn record(kind: u16, version: u16, instance: u16, body: &[u8]) -> Vec<u8> {
    let mut output = Vec::new();
    output.extend_from_slice(&((instance << 4) | version).to_le_bytes());
    output.extend_from_slice(&kind.to_le_bytes());
    output.extend_from_slice(&(body.len() as u32).to_le_bytes());
    output.extend_from_slice(body);
    output
}

fn ppt() -> Vec<u8> {
    let mut slide_body = record(0x0F9F, 0, 0, &0u32.to_le_bytes());
    slide_body.extend_from_slice(&record(0x0FA8, 0, 0, b"Format Matrix PPT"));
    let slide = record(0x03EE, 0xF, 0, &slide_body);

    let mut notes_body = record(0x03F1, 0, 0, &7u32.to_le_bytes());
    notes_body.extend_from_slice(&record(0x0F9F, 0, 0, &2u32.to_le_bytes()));
    notes_body.extend_from_slice(&record(0x0FA8, 0, 0, b"Legacy speaker note"));
    let notes = record(0x03F0, 0xF, 0, &notes_body);

    let mut persist_body = Vec::new();
    persist_body.extend_from_slice(&(1u32 | (1u32 << 20)).to_le_bytes());
    persist_body.extend_from_slice(&0u32.to_le_bytes());
    let persist = record(0x1772, 0, 0, &persist_body);

    let mut slide_persist = Vec::new();
    slide_persist.extend_from_slice(&1u32.to_le_bytes());
    slide_persist.extend_from_slice(&0u32.to_le_bytes());
    slide_persist.extend_from_slice(&1u32.to_le_bytes());
    slide_persist.extend_from_slice(&7u32.to_le_bytes());
    let slide_list = record(0x0FF0, 0xF, 0, &record(0x03F3, 0, 0, &slide_persist));
    let document = record(0x03E8, 0xF, 0, &slide_list);

    let mut stream = Vec::new();
    stream.extend_from_slice(&persist);
    stream.extend_from_slice(&document);
    let slide_offset = stream.len() as u32;
    stream.extend_from_slice(&slide);
    stream.extend_from_slice(&notes);
    stream[12..16].copy_from_slice(&slide_offset.to_le_bytes());
    ole(&[("PowerPoint Document", stream)])
}

fn docx() -> Vec<u8> {
    zip_fixture(&[
        ("[Content_Types].xml", br#"<Types><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/><Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/></Types>"#),
        ("_rels/.rels", br#"<Relationships><Relationship Id="r0" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#),
        ("word/_rels/document.xml.rels", br#"<Relationships><Relationship Id="rH" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/><Relationship Id="rF" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/></Relationships>"#),
        ("word/styles.xml", br#"<w:styles xmlns:w="w"><w:style w:type="paragraph" w:styleId="H"><w:name w:val="Heading 1"/></w:style></w:styles>"#),
        ("word/header1.xml", br#"<w:hdr xmlns:w="w"><w:p><w:r><w:t>Matrix running header</w:t></w:r></w:p></w:hdr>"#),
        ("word/footer1.xml", br#"<w:ftr xmlns:w="w"><w:p><w:r><w:t>Matrix running footer</w:t></w:r></w:p></w:ftr>"#),
        ("word/document.xml", br#"<w:document xmlns:w="w"><w:body><w:p><w:pPr><w:pStyle w:val="H"/></w:pPr><w:r><w:t>Format Matrix DOCX</w:t></w:r></w:p><w:p><w:r><w:t>Source semantic body</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:t>Name</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Value</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:p><w:r><w:t>alpha</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>42</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:body></w:document>"#),
    ])
}

fn pptx() -> Vec<u8> {
    zip_fixture(&[
        ("[Content_Types].xml", br#"<Types><Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/></Types>"#),
        ("_rels/.rels", br#"<Relationships><Relationship Id="r0" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/></Relationships>"#),
        ("ppt/presentation.xml", br#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rS"/></p:sldIdLst></p:presentation>"#),
        ("ppt/_rels/presentation.xml.rels", br#"<Relationships><Relationship Id="rS" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#),
        ("ppt/slides/slide1.xml", br#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>Format Matrix PPTX</a:t></a:r></a:p><a:p><a:r><a:t>Presentation body</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#),
    ])
}

fn xlsx() -> Vec<u8> {
    zip_fixture(&[
        ("[Content_Types].xml", br#"<Types><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#),
        ("_rels/.rels", br#"<Relationships><Relationship Id="r0" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#),
        ("xl/workbook.xml", br#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Matrix" sheetId="1" r:id="rS"/></sheets></workbook>"#),
        ("xl/_rels/workbook.xml.rels", br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rS" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#),
        ("xl/worksheets/sheet1.xml", br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Name</t></is></c><c r="B1" t="inlineStr"><is><t>Value</t></is></c></row><row r="2"><c r="A2" t="inlineStr"><is><t>alpha</t></is></c><c r="B2"><v>42</v></c></row></sheetData></worksheet>"#),
    ])
}

fn odf(mimetype: &'static [u8], body: &'static [u8]) -> Vec<u8> {
    zip_fixture(&[("mimetype", mimetype), ("content.xml", body)])
}

fn epub() -> Vec<u8> {
    zip_fixture(&[
        ("mimetype", b"application/epub+zip"),
        ("META-INF/container.xml", br#"<container><rootfiles><rootfile full-path="book.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#),
        ("book.opf", br#"<package xmlns:dc="dc"><metadata><dc:title>Format Matrix EPUB</dc:title></metadata><manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="chapter"/></spine></package>"#),
        ("chapter.xhtml", br#"<html xmlns="http://www.w3.org/1999/xhtml"><body><h1>Format Matrix EPUB</h1><p>Book chapter body.</p><table><tr><th>Name</th><th>Value</th></tr><tr><td>alpha</td><td>42</td></tr></table></body></html>"#),
    ])
}

fn image(format: ImageFormat) -> Vec<u8> {
    let image = ImageBuffer::from_fn(64, 48, |x, y| {
        if (8..56).contains(&x) && (16..32).contains(&y) {
            Rgb([20u8, 70, 180])
        } else {
            Rgb([245u8, 245, 245])
        }
    });
    let mut output = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut output, format)
        .unwrap();
    output.into_inner()
}

fn write_fixture(root: &Path, name: &str, bytes: &[u8]) {
    fs::write(root.join(name), bytes).unwrap();
    println!("{name}\t{}", bytes.len());
}

fn main() {
    let root = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("benchmark/format_matrix/fixtures"));
    fs::create_dir_all(&root).unwrap();

    write_fixture(&root, "01-pdf.pdf", &pdf());
    write_fixture(&root, "02-doc.doc", &doc());
    write_fixture(&root, "03-docx.docx", &docx());
    write_fixture(&root, "04-ppt.ppt", &ppt());
    write_fixture(&root, "05-pptx.pptx", &pptx());
    write_fixture(&root, "06-excel.xlsx", &xlsx());
    write_fixture(
        &root,
        "07-odt.odt",
        &odf(
            b"application/vnd.oasis.opendocument.text",
            br#"<office:document-content xmlns:office="office" xmlns:text="text" xmlns:table="table"><office:body><office:text><text:h text:outline-level="1">Format Matrix ODT</text:h><text:p>Text document body.</text:p><table:table><table:table-row><table:table-cell><text:p>Name</text:p></table:table-cell><table:table-cell><text:p>Value</text:p></table:table-cell></table:table-row></table:table></office:text></office:body></office:document-content>"#,
        ),
    );
    write_fixture(
        &root,
        "08-ods.ods",
        &odf(
            b"application/vnd.oasis.opendocument.spreadsheet",
            br#"<office:document-content xmlns:office="office" xmlns:text="text" xmlns:table="table"><office:body><office:spreadsheet><table:table table:name="Matrix"><table:table-row><table:table-cell><text:p>Name</text:p></table:table-cell><table:table-cell><text:p>Value</text:p></table:table-cell></table:table-row><table:table-row><table:table-cell><text:p>alpha</text:p></table:table-cell><table:table-cell office:value-type="float" office:value="42"><text:p>42</text:p></table:table-cell></table:table-row></table:table></office:spreadsheet></office:body></office:document-content>"#,
        ),
    );
    write_fixture(
        &root,
        "09-odp.odp",
        &odf(
            b"application/vnd.oasis.opendocument.presentation",
            br#"<office:document-content xmlns:office="office" xmlns:text="text" xmlns:draw="draw" xmlns:presentation="presentation"><office:body><office:presentation><draw:page draw:name="Slide 1"><draw:frame presentation:class="title"><draw:text-box><text:p>Format Matrix ODP</text:p></draw:text-box></draw:frame><draw:frame presentation:class="outline"><draw:text-box><text:p>Presentation body</text:p></draw:text-box></draw:frame></draw:page></office:presentation></office:body></office:document-content>"#,
        ),
    );
    write_fixture(
        &root,
        "10-rtf.rtf",
        br#"{\rtf1\ansi Format Matrix RTF\par Body text{\footnote Matrix note}\par}"#,
    );
    write_fixture(&root, "11-epub.epub", &epub());
    write_fixture(&root, "12-csv.csv", b"name,value\nalpha,42\nbeta,7\n");
    write_fixture(&root, "13-tsv.tsv", b"name\tvalue\nalpha\t42\nbeta\t7\n");
    write_fixture(&root, "14-png.png", &image(ImageFormat::Png));
    write_fixture(&root, "15-jpeg.jpg", &image(ImageFormat::Jpeg));
    write_fixture(
        &root,
        "16-unknown.bin",
        b"uparser format matrix unknown payload\n",
    );
}
