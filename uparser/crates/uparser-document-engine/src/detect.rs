use crate::ParseOptions;
use crate::ooxml::{ContentTypes, load_root_relationships, main_part};
use crate::package::Package;
use file_format::FileFormat;
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
        // A ZIP whose OPC/ODF identity we could not establish: trust the
        // filename over a generic archive sniffer, which reports OOXML-shaped
        // archives as whichever family it happens to guess first.
        if let Some(format) = extension_format(filename_hint) {
            return format;
        }
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

    extension_format(filename_hint).unwrap_or(DocumentFormat::Unknown)
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

fn extension_format(filename_hint: Option<&str>) -> Option<DocumentFormat> {
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

    // An EPUB without a readable `mimetype` is still identifiable by its
    // mandatory OCF container part.
    if package.names().any(|name| name == "META-INF/container.xml") {
        return Some(DocumentFormat::Epub);
    }
    None
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
    }

    #[test]
    fn content_wins_over_misleading_extension() {
        assert_eq!(
            detect_format(b"%PDF-1.7", Some("data.csv")),
            DocumentFormat::Pdf
        );
    }
}
