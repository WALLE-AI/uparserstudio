mod csv;
mod doc;
mod docx;
mod epub;
mod odf;
mod ppt;
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
        // XLS/XLSX/XLSM/XLSB go through calamine; ODS goes through the ODF
        // walker instead, which is the only path that understands ODF's
        // `number-*-repeated` / `covered-table-cell` encoding and charges it
        // against the expansion budget.
        DocumentFormat::Excel => sheet::parse(bytes, format, options),
        DocumentFormat::Doc => doc::parse(bytes, options),
        DocumentFormat::Docx => docx::parse(bytes, options),
        DocumentFormat::Ppt => ppt::parse(bytes, options),
        DocumentFormat::Pptx => pptx::parse(bytes, options),
        DocumentFormat::Odt | DocumentFormat::Odp | DocumentFormat::Ods => {
            odf::parse(bytes, format, options)
        }
        DocumentFormat::Epub => epub::parse(bytes, options),
        DocumentFormat::Rtf => rtf::parse(bytes, options),
        _ => Err(DocumentError::UnsupportedFormat(format)),
    }
}
