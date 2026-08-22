use crate::ParseOptions;
use crate::ooxml::{ContentTypes, load_root_relationships, main_part};
use crate::package::Package;
use encoding_rs::{UTF_8, UTF_16BE, UTF_16LE};
use file_format::FileFormat;
use quick_xml::Reader;
use quick_xml::events::Event;
use serde::{Deserialize, Serialize};
use std::io::Cursor;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentFormat {
    Pdf,
    Doc,
    Docx,
    Ppt,
    Pptx,
    Excel,
    Odt,
    Ods,
    Odp,
    Rtf,
    Epub,
    Csv,
    Tsv,
    Png,
    Jpeg,
    Unknown,
}

impl DocumentFormat {
    pub const ALL: [Self; 16] = [
        Self::Pdf,
        Self::Doc,
        Self::Docx,
        Self::Ppt,
        Self::Pptx,
        Self::Excel,
        Self::Odt,
        Self::Ods,
        Self::Odp,
        Self::Rtf,
        Self::Epub,
        Self::Csv,
        Self::Tsv,
        Self::Png,
        Self::Jpeg,
        Self::Unknown,
    ];

    pub const fn is_recognized(self) -> bool {
        !matches!(self, Self::Unknown)
    }
}

pub fn detect_format(bytes: &[u8], filename_hint: Option<&str>) -> DocumentFormat {
    if bytes.starts_with(b"{\\rtf") {
        return DocumentFormat::Rtf;
    }

    if bytes.starts_with(&[0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1])
        && let Some(format) = detect_ole_package(bytes)
    {
        return format;
    }

    if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06") {
        if let Some(format) = detect_zip_package(bytes) {
            return format;
        }
        return DocumentFormat::Unknown;
    }

    let detected = FileFormat::from_bytes(bytes);
    let from_signature = match detected.extension().to_ascii_lowercase().as_str() {
        "pdf" => DocumentFormat::Pdf,
        "doc" => DocumentFormat::Doc,
        "docx" => DocumentFormat::Docx,
        "ppt" => DocumentFormat::Ppt,
        "pptx" => DocumentFormat::Pptx,
        "xls" | "xlsx" | "xlsb" | "xlsm" | "xla" | "xlam" => DocumentFormat::Excel,
        "odt" => DocumentFormat::Odt,
        "ods" => DocumentFormat::Ods,
        "odp" => DocumentFormat::Odp,
        "epub" => DocumentFormat::Epub,
        "rtf" => DocumentFormat::Rtf,
        "png" => DocumentFormat::Png,
        "jpg" | "jpeg" => DocumentFormat::Jpeg,
        _ => DocumentFormat::Unknown,
    };
    if from_signature != DocumentFormat::Unknown {
        return from_signature;
    }

    detect_delimited_text(bytes, filename_hint).unwrap_or(DocumentFormat::Unknown)
}

fn detect_ole_package(bytes: &[u8]) -> Option<DocumentFormat> {
    let compound = cfb::CompoundFile::open(Cursor::new(bytes)).ok()?;
    if compound.exists("/WordDocument") {
        Some(DocumentFormat::Doc)
    } else if compound.exists("/PowerPoint Document") {
        Some(DocumentFormat::Ppt)
    } else if compound.exists("/Workbook") || compound.exists("/Book") {
        Some(DocumentFormat::Excel)
    } else {
        None
    }
}

pub fn format_from_extension(filename_hint: Option<&str>) -> Option<DocumentFormat> {
    let extension = filename_hint?
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())?;
    Some(match extension.as_str() {
        "pdf" => DocumentFormat::Pdf,
        "doc" => DocumentFormat::Doc,
        "docx" => DocumentFormat::Docx,
        "ppt" => DocumentFormat::Ppt,
        "pptx" => DocumentFormat::Pptx,
        "xls" | "xlsx" | "xlsb" | "xlsm" | "xla" | "xlam" => DocumentFormat::Excel,
        "odt" => DocumentFormat::Odt,
        "ods" => DocumentFormat::Ods,
        "odp" => DocumentFormat::Odp,
        "rtf" => DocumentFormat::Rtf,
        "epub" => DocumentFormat::Epub,
        "csv" => DocumentFormat::Csv,
        "tsv" | "tab" => DocumentFormat::Tsv,
        "png" => DocumentFormat::Png,
        "jpg" | "jpeg" => DocumentFormat::Jpeg,
        _ => return None,
    })
}

fn detect_delimited_text(bytes: &[u8], filename_hint: Option<&str>) -> Option<DocumentFormat> {
    let format = match format_from_extension(filename_hint)? {
        DocumentFormat::Csv => DocumentFormat::Csv,
        DocumentFormat::Tsv => DocumentFormat::Tsv,
        _ => return None,
    };
    let text = decode_delimited_text(bytes)?;
    let delimiter = match format {
        DocumentFormat::Csv => sniff_csv_delimiter(text.as_bytes())?,
        DocumentFormat::Tsv => text.as_bytes().contains(&b'\t').then_some(b'\t')?,
        _ => return None,
    };

    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(false)
        .from_reader(text.as_bytes());
    let mut width = None;
    let mut records = 0usize;
    for record in reader.records().take(64) {
        let record = record.ok()?;
        if record.len() < 2 {
            return None;
        }
        if let Some(expected) = width {
            if record.len() != expected {
                return None;
            }
        } else {
            width = Some(record.len());
        }
        records += 1;
    }
    (records > 0).then_some(format)
}

fn decode_delimited_text(bytes: &[u8]) -> Option<std::borrow::Cow<'_, str>> {
    let (text, had_errors) = if bytes.starts_with(&[0xff, 0xfe]) {
        let (text, _, had_errors) = UTF_16LE.decode(&bytes[2..]);
        (text, had_errors)
    } else if bytes.starts_with(&[0xfe, 0xff]) {
        let (text, _, had_errors) = UTF_16BE.decode(&bytes[2..]);
        (text, had_errors)
    } else {
        let source = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
        let (text, _, had_errors) = UTF_8.decode(source);
        (text, had_errors)
    };
    (!had_errors).then_some(text)
}

fn sniff_csv_delimiter(bytes: &[u8]) -> Option<u8> {
    [b',', b';', b'|']
        .into_iter()
        .map(|delimiter| (delimiter_consistency(bytes, delimiter), delimiter))
        .filter(|((rows, width), _)| *rows > 0 && *width > 0)
        .max()
        .map(|(_, delimiter)| delimiter)
}

fn delimiter_consistency(bytes: &[u8], delimiter: u8) -> (usize, usize) {
    let counts = bytes
        .split(|byte| *byte == b'\n')
        .take(64)
        .map(|line| line.iter().filter(|byte| **byte == delimiter).count())
        .filter(|count| *count > 0)
        .collect::<Vec<_>>();
    let Some(&expected) = counts.first() else {
        return (0, 0);
    };
    let consistent = counts.iter().filter(|count| **count == expected).count();
    (consistent, expected)
}

/// Classify a ZIP container.
///
/// ODF and EPUB self-identify through their `mimetype` entry, so they are
/// checked first and exactly. OOXML has no such marker, and its
/// `[Content_Types].xml` may legitimately declare parts belonging to *other*
/// families (an embedded Word object inside a deck declares
/// `wordprocessingml` content types). Substring-scanning that file therefore
/// misclassifies real packages; the only authoritative route is the root
/// `officeDocument` relationship → main part → that part's content type.
fn detect_zip_package(bytes: &[u8]) -> Option<DocumentFormat> {
    let options = ParseOptions::default();
    let mut package = Package::open(bytes, &options.limits).ok()?;

    if let Ok(Some(mimetype)) = package.read("mimetype") {
        let mimetype = String::from_utf8_lossy(&mimetype)
            .trim()
            .to_ascii_lowercase();
        match mimetype.as_str() {
            "application/vnd.oasis.opendocument.text"
            | "application/vnd.oasis.opendocument.text-template" => {
                return Some(DocumentFormat::Odt);
            }
            "application/vnd.oasis.opendocument.spreadsheet"
            | "application/vnd.oasis.opendocument.spreadsheet-template" => {
                return Some(DocumentFormat::Ods);
            }
            "application/vnd.oasis.opendocument.presentation"
            | "application/vnd.oasis.opendocument.presentation-template" => {
                return Some(DocumentFormat::Odp);
            }
            "application/epub+zip" => return Some(DocumentFormat::Epub),
            _ => {}
        }
    }

    if let Some(format) = ooxml_family(&mut package, &options) {
        return Some(format);
    }

    let names: Vec<String> = package.names().map(str::to_owned).collect();
    if names.iter().any(|name| name.starts_with("word/")) {
        return Some(DocumentFormat::Docx);
    }
    if names.iter().any(|name| name.starts_with("ppt/")) {
        return Some(DocumentFormat::Pptx);
    }
    if names.iter().any(|name| name.starts_with("xl/")) {
        return Some(DocumentFormat::Excel);
    }

    // Preserve a typed "missing main part" error for malformed OOXML that
    // still declares exactly one Office family in its container metadata.
    if let Some(format) = declared_ooxml_family(&mut package) {
        return Some(format);
    }

    // An EPUB without a readable `mimetype` is still identifiable by its
    // mandatory OCF container part.
    if package.names().any(|name| name == "META-INF/container.xml") {
        return Some(DocumentFormat::Epub);
    }
    None
}

fn declared_ooxml_family(package: &mut Package<'_>) -> Option<DocumentFormat> {
    let xml = package.read("[Content_Types].xml").ok()??;
    let mut reader = Reader::from_reader(xml.as_slice());
    let mut declared = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(event) | Event::Empty(event)) => {
                for attribute in event.attributes().flatten() {
                    if attribute.key.local_name().as_ref() != b"ContentType" {
                        continue;
                    }
                    let value = std::str::from_utf8(attribute.value.as_ref()).ok()?;
                    let Some(format) = family_from_content_type(Some(value)) else {
                        continue;
                    };
                    if declared.is_some_and(|existing| existing != format) {
                        return None;
                    }
                    declared = Some(format);
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => return None,
            _ => {}
        }
    }
    declared
}

/// Resolve the OOXML main part and classify by its declared content type.
fn ooxml_family(package: &mut Package<'_>, options: &ParseOptions) -> Option<DocumentFormat> {
    let content_types = ContentTypes::load(package, options).ok()?;
    let root_relationships = load_root_relationships(package, options).ok()?;

    let mut candidates = Vec::new();
    if let Some(part) = main_part(&root_relationships) {
        candidates.push(part);
    }
    // Conventional fallbacks for packages with a missing or broken root rels
    // part. Only consulted when the part actually exists in the archive, so a
    // stray content-type override cannot select a family on its own.
    for conventional in [
        "word/document.xml",
        "ppt/presentation.xml",
        "xl/workbook.xml",
        "xl/workbook.bin",
    ] {
        candidates.push(conventional.to_owned());
    }

    let present: Vec<String> = package.names().map(str::to_owned).collect();
    for candidate in candidates {
        if !present.iter().any(|name| name == &candidate) {
            continue;
        }
        if let Some(format) = family_from_content_type(content_types.for_part(&candidate)) {
            return Some(format);
        }
        // The part exists but declares no usable content type: fall back to
        // its conventional location.
        if let Some(format) = family_from_conventional_path(&candidate) {
            return Some(format);
        }
    }
    None
}

fn family_from_content_type(content_type: Option<&str>) -> Option<DocumentFormat> {
    let content_type = content_type?.to_ascii_lowercase();
    if content_type.contains("wordprocessingml") || content_type.contains("ms-word") {
        Some(DocumentFormat::Docx)
    } else if content_type.contains("presentationml") || content_type.contains("ms-powerpoint") {
        Some(DocumentFormat::Pptx)
    } else if content_type.contains("spreadsheetml") || content_type.contains("ms-excel") {
        Some(DocumentFormat::Excel)
    } else {
        None
    }
}

fn family_from_conventional_path(part: &str) -> Option<DocumentFormat> {
    if part.starts_with("word/") {
        Some(DocumentFormat::Docx)
    } else if part.starts_with("ppt/") {
        Some(DocumentFormat::Pptx)
    } else if part.starts_with("xl/") {
        Some(DocumentFormat::Excel)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_distinguishes_delimited_text() {
        assert_eq!(
            detect_format(b"a,b\n1,2", Some("data.csv")),
            DocumentFormat::Csv
        );
        assert_eq!(
            detect_format(b"a\tb\n1\t2", Some("data.tsv")),
            DocumentFormat::Tsv
        );
        assert_eq!(
            detect_format(b"a;b;c\n\"1,5\";\"2,5\";x\n", Some("data.csv")),
            DocumentFormat::Csv
        );
    }

    #[test]
    fn detects_utf16_delimited_text() {
        let mut bytes = vec![0xff, 0xfe];
        bytes.extend(
            "col1,col2\nnaive,cafe\n"
                .encode_utf16()
                .flat_map(u16::to_le_bytes),
        );
        assert_eq!(detect_format(&bytes, Some("data.csv")), DocumentFormat::Csv);
    }

    #[test]
    fn content_wins_over_misleading_extension() {
        assert_eq!(
            detect_format(b"%PDF-1.7", Some("data.csv")),
            DocumentFormat::Pdf
        );
    }

    #[test]
    fn arbitrary_bytes_do_not_become_a_format_from_extension() {
        assert_eq!(
            detect_format(b"not a pdf", Some("document.pdf")),
            DocumentFormat::Unknown
        );
        assert_eq!(
            detect_format(b"PK\x03\x04broken", Some("document.docx")),
            DocumentFormat::Unknown
        );
    }

    #[test]
    fn malformed_delimited_text_is_unknown() {
        assert_eq!(
            detect_format(b"a,b\n1\n", Some("data.csv")),
            DocumentFormat::Unknown
        );
        assert_eq!(
            detect_format(b"plain text", Some("data.csv")),
            DocumentFormat::Unknown
        );
    }
}
