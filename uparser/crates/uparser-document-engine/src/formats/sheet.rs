use crate::package::Package;
use crate::{
    Block, CanonicalDocument, Cell, CellSlot, CellValueKind, DocumentError, DocumentFormat,
    DocumentUnit, FormulaSource, ParseOptions, ParseWarning, Table, TableKind, UnitKind,
    WarningCode,
};
use calamine::{Data, Dimensions, Range, Reader, Sheets, open_workbook_auto_from_rs};
use std::io::Cursor;

pub(crate) fn parse(
    bytes: &[u8],
    format: DocumentFormat,
    options: &ParseOptions,
) -> Result<CanonicalDocument, DocumentError> {
    if bytes.starts_with(b"PK") {
        Package::open(bytes, &options.limits)?;
    }
    let cursor = Cursor::new(bytes);
    let mut workbook = open_workbook_auto_from_rs(cursor)
        .map_err(|error| DocumentError::malformed(error.to_string()))?;
    let names = workbook.sheet_names();
    let mut document = CanonicalDocument::new(format);
    document.metadata.variant = Some(workbook_variant(&workbook).to_owned());

    for (index, name) in names.into_iter().enumerate() {
        let merges = merge_cells(&mut workbook, &name, &mut document.warnings);
        let formulas = workbook.worksheet_formula(&name).ok();
        let range = workbook
            .worksheet_range(&name)
            .map_err(|error| DocumentError::Malformed {
                part: Some(name.clone()),
                detail: error.to_string(),
            })?;
        let cells = (range.height() as u64).saturating_mul(range.width() as u64);
        if cells > options.limits.max_expansion {
            return Err(DocumentError::ResourceLimit {
                limit: "max_expansion",
                detail: format!("worksheet {name:?} expands to {cells} cells"),
            });
        }
        let table = build_table(
            &range,
            formulas.as_ref(),
            &merges,
            &mut document.warnings,
            &name,
        );
        let mut unit = DocumentUnit::new(UnitKind::Sheet, index, Some(name));
        unit.blocks.push(Block::Table { table });
        document.units.push(unit);
    }
    Ok(document)
}

fn workbook_variant<RS>(workbook: &Sheets<RS>) -> &'static str {
    match workbook {
        Sheets::Xls(_) => "xls",
        Sheets::Xlsx(_) => "xlsx",
        Sheets::Xlsb(_) => "xlsb",
        Sheets::Ods(_) => "ods",
    }
}

fn merge_cells<RS: std::io::Read + std::io::Seek>(
    workbook: &mut Sheets<RS>,
    name: &str,
    warnings: &mut Vec<ParseWarning>,
) -> Vec<Dimensions> {
    match workbook {
        Sheets::Xls(book) => book.worksheet_merge_cells(name).unwrap_or_default(),
        Sheets::Xlsx(book) => match book.worksheet_merge_cells(name) {
            Some(Ok(merges)) => merges,
            Some(Err(error)) => {
                warnings.push(ParseWarning {
                    code: WarningCode::OptionalPartSkipped,
                    part: Some(name.to_owned()),
                    message: format!("could not read merged cells: {error}"),
                });
                Vec::new()
            }
            None => Vec::new(),
        },
        Sheets::Xlsb(_) | Sheets::Ods(_) => Vec::new(),
    }
}

fn build_table(
    range: &Range<Data>,
    formulas: Option<&Range<String>>,
    merges: &[Dimensions],
    warnings: &mut Vec<ParseWarning>,
    sheet_name: &str,
) -> Table {
    let rows = range.height();
    let columns = range.width();
    let start = range.start().unwrap_or((0, 0));
    let mut grid = Vec::with_capacity(rows);
    for row in 0..rows {
        let mut output_row = Vec::with_capacity(columns);
        for column in 0..columns {
            let absolute = (start.0 + row as u32, start.1 + column as u32);
            let data = range.get((row, column)).unwrap_or(&Data::Empty);
            let (text, value_kind) = data_value(data);
            let formula = formulas
                .and_then(|items| items.get_value(absolute))
                .filter(|value| !value.is_empty())
                .map(|value| FormulaSource::Spreadsheet(value.clone()));
            let mut cell = Cell::text(text, value_kind);
            cell.formula = formula;
            output_row.push(CellSlot::Origin(cell));
        }
        grid.push(output_row);
    }

    for merge in merges {
        if merge.start.0 < start.0 || merge.start.1 < start.1 {
            warnings.push(invalid_merge_warning(sheet_name, merge));
            continue;
        }
        let origin_row = (merge.start.0 - start.0) as usize;
        let origin_column = (merge.start.1 - start.1) as usize;
        let end_row = (merge.end.0.saturating_sub(start.0)) as usize;
        let end_column = (merge.end.1.saturating_sub(start.1)) as usize;
        if origin_row >= rows
            || origin_column >= columns
            || end_row >= rows
            || end_column >= columns
        {
            warnings.push(invalid_merge_warning(sheet_name, merge));
            continue;
        }
        if let CellSlot::Origin(cell) = &mut grid[origin_row][origin_column] {
            cell.row_span = end_row - origin_row + 1;
            cell.column_span = end_column - origin_column + 1;
        }
        for (row_index, row_slots) in grid
            .iter_mut()
            .enumerate()
            .take(end_row + 1)
            .skip(origin_row)
        {
            for (column_index, slot) in row_slots
                .iter_mut()
                .enumerate()
                .take(end_column + 1)
                .skip(origin_column)
            {
                if row_index != origin_row || column_index != origin_column {
                    *slot = CellSlot::Covered {
                        origin_row,
                        origin_column,
                    };
                }
            }
        }
    }

    Table {
        kind: TableKind::Data,
        rows,
        columns,
        header_rows: infer_header_rows(range),
        grid,
        caption: None,
    }
}

fn invalid_merge_warning(sheet_name: &str, merge: &Dimensions) -> ParseWarning {
    ParseWarning {
        code: WarningCode::InvalidSpanClamped,
        part: Some(sheet_name.to_owned()),
        message: format!(
            "ignored merged range {:?}:{:?} outside the used grid",
            merge.start, merge.end
        ),
    }
}

fn infer_header_rows(range: &Range<Data>) -> usize {
    let Some(first) = range.rows().next() else {
        return 0;
    };
    let Some(second) = range.rows().nth(1) else {
        return 0;
    };
    let text_headers = first
        .iter()
        .filter(|value| matches!(value, Data::String(text) if !text.trim().is_empty()))
        .count();
    let typed_data = second
        .iter()
        .filter(|value| {
            matches!(
                value,
                Data::Int(_)
                    | Data::Float(_)
                    | Data::Bool(_)
                    | Data::DateTime(_)
                    | Data::DateTimeIso(_)
            )
        })
        .count();
    usize::from(text_headers == first.len() && typed_data > 0)
}

fn data_value(data: &Data) -> (String, CellValueKind) {
    match data {
        Data::Empty => (String::new(), CellValueKind::Empty),
        Data::String(value) => (value.clone(), CellValueKind::Text),
        Data::Int(value) => (value.to_string(), CellValueKind::Number),
        Data::Float(value) => (format_number(*value), CellValueKind::Number),
        // Spreadsheets render booleans uppercase; `true`/`false` is Rust's
        // spelling, not the workbook's.
        Data::Bool(value) => (
            if *value { "TRUE" } else { "FALSE" }.to_owned(),
            CellValueKind::Boolean,
        ),
        // A date/duration cell holds a serial number. Emitting it raw
        // (`46096`, `1.10434027777778`) loses the value a reader sees, so it
        // is rendered back to the ISO date or an elapsed-time string.
        Data::DateTime(value) => (format_datetime(value), CellValueKind::DateTime),
        Data::DateTimeIso(value) | Data::DurationIso(value) => {
            (value.clone(), CellValueKind::DateTime)
        }
        Data::Error(value) => (value.to_string(), CellValueKind::Error),
    }
}

/// Format a float the way a spreadsheet shows it: no exponent, no trailing
/// zeros, and no artefacts of binary rounding.
fn format_number(value: f64) -> String {
    if value == value.trunc() && value.abs() < 1e15 {
        return format!("{}", value as i64);
    }
    let mut text = format!("{value:.10}");
    while text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    text
}

fn format_datetime(value: &calamine::ExcelDateTime) -> String {
    if value.is_duration() {
        // Durations may legitimately exceed 24 hours, so they are formatted as
        // elapsed `[h]:mm:ss` rather than a clock time.
        let total_seconds = (value.as_f64() * 86_400.0).round().max(0.0) as u64;
        return format!(
            "{}:{:02}:{:02}",
            total_seconds / 3600,
            (total_seconds % 3600) / 60,
            total_seconds % 60
        );
    }
    match value.as_datetime() {
        Some(datetime) => {
            // `NaiveDateTime`'s Display is `YYYY-MM-DD HH:MM:SS`; a pure date
            // cell should not carry a midnight time component.
            let text = datetime.to_string();
            text.strip_suffix(" 00:00:00")
                .map(str::to_owned)
                .unwrap_or(text)
        }
        None => format_number(value.as_f64()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use calamine::{CellErrorType, ExcelDateTime, ExcelDateTimeType};

    #[test]
    fn table_preserves_types_formulas_and_merged_cells() {
        let mut values = Range::new((2, 3), (3, 5));
        values.set_value((2, 3), Data::String("Name".to_owned()));
        values.set_value((2, 4), Data::String("Active".to_owned()));
        values.set_value((2, 5), Data::String("Score".to_owned()));
        values.set_value((3, 3), Data::Int(7));
        values.set_value((3, 4), Data::Bool(true));
        values.set_value((3, 5), Data::Float(2.5));
        let mut formulas = Range::new((2, 3), (3, 5));
        formulas.set_value((3, 5), "=1+1.5".to_owned());
        let merges = [Dimensions::new((2, 3), (2, 4))];
        let mut warnings = Vec::new();

        let table = build_table(&values, Some(&formulas), &merges, &mut warnings, "Metrics");

        assert_eq!((table.rows, table.columns, table.header_rows), (2, 3, 1));
        assert!(warnings.is_empty());
        let CellSlot::Origin(header) = &table.grid[0][0] else {
            panic!("merge origin must remain a cell")
        };
        assert_eq!((header.row_span, header.column_span), (1, 2));
        assert!(matches!(
            table.grid[0][1],
            CellSlot::Covered {
                origin_row: 0,
                origin_column: 0
            }
        ));
        let CellSlot::Origin(score) = &table.grid[1][2] else {
            panic!("score must remain an origin cell")
        };
        assert_eq!(score.blocks, vec![Block::paragraph("2.5")]);
        assert_eq!(score.value_kind, CellValueKind::Number);
        assert_eq!(
            score.formula,
            Some(FormulaSource::Spreadsheet("=1+1.5".to_owned()))
        );
    }

    #[test]
    fn merges_outside_the_used_range_are_warned_and_ignored() {
        let mut values = Range::new((4, 4), (5, 5));
        values.set_value((4, 4), Data::String("A".to_owned()));
        values.set_value((5, 5), Data::Int(1));
        let merges = [
            Dimensions::new((3, 4), (4, 4)),
            Dimensions::new((4, 4), (6, 5)),
        ];
        let mut warnings = Vec::new();

        let table = build_table(&values, None, &merges, &mut warnings, "Offset");

        assert_eq!(warnings.len(), 2);
        assert!(warnings.iter().all(|warning| {
            warning.code == WarningCode::InvalidSpanClamped
                && warning.part.as_deref() == Some("Offset")
        }));
        assert!(matches!(table.grid[0][0], CellSlot::Origin(_)));
    }

    #[test]
    fn cell_values_have_stable_user_visible_rendering() {
        let cases = [
            (Data::Empty, "", CellValueKind::Empty),
            (Data::String("text".to_owned()), "text", CellValueKind::Text),
            (Data::Int(-2), "-2", CellValueKind::Number),
            (Data::Float(3.25), "3.25", CellValueKind::Number),
            (Data::Bool(false), "FALSE", CellValueKind::Boolean),
            (
                Data::DateTimeIso("2026-08-22".to_owned()),
                "2026-08-22",
                CellValueKind::DateTime,
            ),
            (
                Data::DurationIso("PT90M".to_owned()),
                "PT90M",
                CellValueKind::DateTime,
            ),
            (
                Data::Error(CellErrorType::Div0),
                "#DIV/0!",
                CellValueKind::Error,
            ),
        ];
        for (input, expected_text, expected_kind) in cases {
            let (text, kind) = data_value(&input);
            assert_eq!(text, expected_text);
            assert_eq!(kind, expected_kind);
        }

        let duration = ExcelDateTime::new(1.5, ExcelDateTimeType::TimeDelta, false);
        assert_eq!(data_value(&Data::DateTime(duration)).0, "36:00:00");
        let date = ExcelDateTime::new(45_292.0, ExcelDateTimeType::DateTime, false);
        assert_eq!(data_value(&Data::DateTime(date)).0, "2024-01-01");
    }

    #[test]
    fn header_inference_and_number_formatting_cover_boundaries() {
        assert_eq!(infer_header_rows(&Range::<Data>::empty()), 0);
        assert_eq!(infer_header_rows(&Range::new((0, 0), (0, 1))), 0);

        let mut values = Range::new((0, 0), (1, 1));
        values.set_value((0, 0), Data::String("Name".to_owned()));
        values.set_value((0, 1), Data::String("Value".to_owned()));
        values.set_value((1, 0), Data::String("alpha".to_owned()));
        values.set_value((1, 1), Data::Int(42));
        assert_eq!(infer_header_rows(&values), 1);

        assert_eq!(format_number(42.0), "42");
        assert_eq!(format_number(-0.125), "-0.125");
        assert_eq!(format_number(1.230_000_000_01), "1.23");
    }
}
