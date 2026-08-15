mod csv;
mod docx;
mod epub;
mod odf;
mod pptx;
mod rtf;
mod sheet;

use crate::{CanonicalDocument, DocumentError, DocumentFormat, ParseOptions};

pub(crate) fn parse(
    bytes: &[u8],
    format: DocumentFormat,
    options: &ParseOptions,
) -> Result<CanonicalDocument, DocumentError> {
    match format {
        DocumentFormat::Csv | DocumentFormat::Tsv => csv::parse(bytes, format, options),
        DocumentFormat::Excel | DocumentFormat::Ods => sheet::parse(bytes, format, options),
        DocumentFormat::Docx => docx::parse(bytes, options),
        DocumentFormat::Pptx => pptx::parse(bytes, options),
        DocumentFormat::Odt | DocumentFormat::Odp => odf::parse(bytes, format, options),
        DocumentFormat::Epub => epub::parse(bytes, options),
        DocumentFormat::Rtf => rtf::parse(bytes, options),
        _ => Err(DocumentError::UnsupportedFormat(format)),
    }
}
