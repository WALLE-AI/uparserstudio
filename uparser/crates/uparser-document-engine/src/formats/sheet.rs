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
        Data::Float(value) => (value.to_string(), CellValueKind::Number),
        Data::Bool(value) => (value.to_string(), CellValueKind::Boolean),
        Data::DateTime(value) => (value.to_string(), CellValueKind::DateTime),
        Data::DateTimeIso(value) | Data::DurationIso(value) => {
            (value.clone(), CellValueKind::DateTime)
        }
        Data::Error(value) => (value.to_string(), CellValueKind::Error),
    }
}
