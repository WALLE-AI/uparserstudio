use crate::{
    Block, CanonicalDocument, Cell, CellSlot, CellValueKind, DocumentError, DocumentFormat,
    DocumentUnit, ParseOptions, Table, TableKind, UnitKind,
};
use encoding_rs::{UTF_8, UTF_16BE, UTF_16LE};

pub(crate) fn parse(
    bytes: &[u8],
    format: DocumentFormat,
    options: &ParseOptions,
) -> Result<CanonicalDocument, DocumentError> {
    let text = decode_text(bytes);
    if text.len() > options.limits.max_text_bytes {
        return Err(DocumentError::ResourceLimit {
            limit: "max_text_bytes",
            detail: format!("decoded text is {} bytes", text.len()),
        });
    }
    let delimiter = if format == DocumentFormat::Tsv {
        b'\t'
    } else {
        sniff_delimiter(text.as_bytes())
    };
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .from_reader(text.as_bytes());
    let mut records = Vec::new();
    let mut columns = 0usize;
    let mut cells = 0u64;
    for record in reader.records() {
        let record = record.map_err(|error| DocumentError::malformed(error.to_string()))?;
        cells = cells.saturating_add(record.len() as u64);
        if cells > options.limits.max_expansion {
            return Err(DocumentError::ResourceLimit {
                limit: "max_expansion",
                detail: format!(
                    "delimited input expands beyond {} cells",
                    options.limits.max_expansion
                ),
            });
        }
        columns = columns.max(record.len());
        records.push(record.iter().map(str::to_owned).collect::<Vec<_>>());
    }

    let header_rows = infer_header_rows(&records);
    let grid = records
        .iter()
        .map(|row| {
            (0..columns)
                .map(|column| {
                    let value = row.get(column).map(String::as_str).unwrap_or_default();
                    let kind = infer_value_kind(value);
                    CellSlot::Origin(Cell::text(value, kind))
                })
                .collect()
        })
        .collect();
    let table = Table {
        kind: TableKind::Data,
        rows: records.len(),
        columns,
        header_rows,
        grid,
        caption: None,
    };
    let mut document = CanonicalDocument::new(format);
    document.metadata.variant = Some(if delimiter == b'\t' { "tsv" } else { "csv" }.to_owned());
    // A delimited-text file has exactly one anonymous table. Naming it
    // "Sheet 1" would inject a heading the source does not contain, which
    // shows up as a spurious document title downstream.
    let mut unit = DocumentUnit::new(UnitKind::Sheet, 0, None);
    unit.blocks.push(Block::Table { table });
    document.units.push(unit);
    Ok(document)
}

fn decode_text(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xff, 0xfe]) {
        UTF_16LE.decode(&bytes[2..]).0.into_owned()
    } else if bytes.starts_with(&[0xfe, 0xff]) {
        UTF_16BE.decode(&bytes[2..]).0.into_owned()
    } else {
        UTF_8
            .decode(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes))
            .0
            .into_owned()
    }
}

fn sniff_delimiter(bytes: &[u8]) -> u8 {
    let candidates = *b",\t;|";
    candidates
        .into_iter()
        .max_by_key(|candidate| delimiter_score(bytes, *candidate))
        .unwrap_or(b',')
}

fn delimiter_score(bytes: &[u8], delimiter: u8) -> (usize, usize) {
    let mut counts = bytes
        .split(|byte| *byte == b'\n')
        .take(20)
        .map(|line| line.iter().filter(|byte| **byte == delimiter).count())
        .filter(|count| *count > 0)
        .collect::<Vec<_>>();
    if counts.is_empty() {
        return (0, 0);
    }
    counts.sort_unstable();
    let median = counts[counts.len() / 2];
    let consistent = counts.iter().filter(|count| **count == median).count();
    (consistent, median)
}

/// Whether the first row is a header.
///
/// Delimited text carries a header row by overwhelming convention, so the
/// default is "yes" and the test is for evidence *against* it — a first row
/// that already contains typed values is data, not labels. The earlier rule
/// required the second row to be typed as well, which classified an
/// all-text table as headerless and pushed its label row into the body.
fn infer_header_rows(rows: &[Vec<String>]) -> usize {
    let Some(first) = rows.first() else { return 0 };
    if rows.len() < 2 {
        return 0;
    }
    let looks_like_data = first.iter().any(|value| {
        matches!(
            infer_value_kind(value),
            CellValueKind::Number | CellValueKind::Boolean | CellValueKind::DateTime
        )
    });
    usize::from(!looks_like_data)
}

fn infer_value_kind(value: &str) -> CellValueKind {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        CellValueKind::Empty
    } else if trimmed.parse::<f64>().is_ok() {
        CellValueKind::Number
    } else if trimmed.eq_ignore_ascii_case("true") || trimmed.eq_ignore_ascii_case("false") {
        CellValueKind::Boolean
    } else {
        CellValueKind::Text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quoted_csv_and_infers_header() {
        let document = parse(
            b"name,value\n\"a,b\",42\n",
            DocumentFormat::Csv,
            &ParseOptions::default(),
        )
        .unwrap();
        let Block::Table { table } = &document.units[0].blocks[0] else {
            panic!()
        };
        assert_eq!((table.rows, table.columns, table.header_rows), (2, 2, 1));
    }

    #[test]
    fn sniffs_semicolon() {
        assert_eq!(sniff_delimiter(b"a;b\n1;2\n"), b';');
    }
}
