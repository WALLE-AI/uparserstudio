use file_format::FileFormat;
use serde::{Deserialize, Serialize};
use std::io::{Cursor, Read};

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

    if (bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06"))
        && let Some(format) = detect_zip_package(bytes)
    {
        return format;
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

fn detect_zip_package(bytes: &[u8]) -> Option<DocumentFormat> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).ok()?;
    let mut evidence = String::new();
    for name in ["[Content_Types].xml", "mimetype", "META-INF/container.xml"] {
        let Ok(mut entry) = archive.by_name(name) else {
            continue;
        };
        let mut limited = entry.by_ref().take(256 * 1024);
        limited.read_to_string(&mut evidence).ok()?;
        evidence.push('\n');
    }
    let evidence = evidence.to_ascii_lowercase();
    if evidence.contains("wordprocessingml.document") {
        Some(DocumentFormat::Docx)
    } else if evidence.contains("presentationml.presentation") {
        Some(DocumentFormat::Pptx)
    } else if evidence.contains("spreadsheetml.sheet")
        || evidence.contains("spreadsheetml.template")
    {
        Some(DocumentFormat::Excel)
    } else if evidence.contains("application/vnd.oasis.opendocument.text") {
        Some(DocumentFormat::Odt)
    } else if evidence.contains("application/vnd.oasis.opendocument.spreadsheet") {
        Some(DocumentFormat::Ods)
    } else if evidence.contains("application/vnd.oasis.opendocument.presentation") {
        Some(DocumentFormat::Odp)
    } else if evidence.contains("application/epub+zip") || evidence.contains("container.xml") {
        Some(DocumentFormat::Epub)
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
