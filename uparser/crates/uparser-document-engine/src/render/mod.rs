use crate::{
    Block, CanonicalDocument, Cell, CellSlot, FormulaSource, Inline, LinkTarget, List, ListMarker,
    NoteKind, Style, Table, UnitKind,
};

pub fn document_json(document: &CanonicalDocument) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(document)
}

pub fn markdown(document: &CanonicalDocument) -> String {
    let mut output = String::new();
    let show_labels = document.units.len() > 1
        || document
            .units
            .first()
            .is_some_and(|unit| unit.kind != UnitKind::Flow);
    for (unit_index, unit) in document.units.iter().enumerate() {
        if show_labels {
            if unit_index > 0 {
                output.push('\n');
            }
            output.push_str("# ");
            output.push_str(unit.label.as_deref().unwrap_or("Untitled"));
            output.push_str("\n\n");
        }
        render_blocks(&unit.blocks, &mut output, 0);
    }
    for note in &document.notes {
        output.push_str("[^");
        output.push_str(&note.id);
        output.push_str("]: ");
        let mut note_text = String::new();
        render_blocks(&note.blocks, &mut note_text, 0);
        output.push_str(note_text.trim());
        if note.kind == NoteKind::Comment {
            output.push_str(" (comment)");
        }
        output.push('\n');
    }
    output.trim_end().to_owned() + "\n"
}

fn render_blocks(blocks: &[Block], output: &mut String, depth: usize) {
    for block in blocks {
        match block {
            Block::Heading { level, content } => {
                output.push_str(&"#".repeat((*level).clamp(1, 6) as usize));
                output.push(' ');
                render_inlines(content, output);
                output.push_str("\n\n");
            }
            Block::Paragraph { content } => {
                render_inlines(content, output);
                output.push_str("\n\n");
            }
            Block::List { list } => render_list(list, output, depth),
            Block::Table { table } if table.has_spans() => render_html_table(table, output),
            Block::Table { table } => render_markdown_table(table, output),
            Block::BlockQuote { blocks } => {
                let mut nested = String::new();
                render_blocks(blocks, &mut nested, depth + 1);
                for line in nested.trim_end().lines() {
                    output.push_str("> ");
                    output.push_str(line);
                    output.push('\n');
                }
                output.push('\n');
            }
            Block::CodeBlock { language, text } => {
                output.push_str("~~~");
                output.push_str(language.as_deref().unwrap_or_default());
                output.push('\n');
                output.push_str(text);
                output.push_str("\n~~~\n\n");
            }
            Block::Figure {
                asset_id,
                alt,
                caption,
            } => {
                output.push_str("![");
                output.push_str(alt.as_deref().unwrap_or_default());
                output.push_str("](");
                output.push_str(asset_id.as_deref().unwrap_or_default());
                output.push(')');
                if !caption.is_empty() {
                    output.push(' ');
                    render_inlines(caption, output);
                }
                output.push_str("\n\n");
            }
            Block::Rule => output.push_str("---\n\n"),
        }
    }
}

fn render_inlines(inlines: &[Inline], output: &mut String) {
    for inline in inlines {
        match inline {
            Inline::Text { text, style } => render_styled_text(text, style, output),
            Inline::Link { target, content } => {
                output.push('[');
                render_inlines(content, output);
                output.push_str("](");
                match target {
                    LinkTarget::External(value) => output.push_str(value),
                    LinkTarget::Anchor(value) => {
                        output.push('#');
                        output.push_str(value);
                    }
                }
                output.push(')');
            }
            Inline::Image { source, alt } => {
                output.push_str("![");
                output.push_str(alt.as_deref().unwrap_or_default());
                output.push_str("](");
                match source {
                    crate::ImageSource::Asset(value) | crate::ImageSource::External(value) => {
                        output.push_str(value)
                    }
                }
                output.push(')');
            }
            Inline::Anchor { id } => {
                output.push_str("<a id=\"");
                output.push_str(&escape_html(id));
                output.push_str("\"></a>");
            }
            Inline::NoteRef { id } => {
                output.push_str("[^");
                output.push_str(id);
                output.push(']');
            }
            Inline::LineBreak => output.push_str("  \n"),
            Inline::Formula { source, display } => match source {
                FormulaSource::Latex(value) => {
                    output.push('$');
                    output.push_str(value);
                    output.push('$');
                }
                _ => output.push_str(display.as_deref().unwrap_or_default()),
            },
        }
    }
}

fn render_styled_text(text: &str, style: &Style, output: &mut String) {
    let escaped = escape_markdown(text);
    if style.code {
        output.push('`');
    }
    if style.bold {
        output.push_str("**");
    }
    if style.italic {
        output.push('*');
    }
    if style.strike {
        output.push_str("~~");
    }
    output.push_str(&escaped);
    if style.strike {
        output.push_str("~~");
    }
    if style.italic {
        output.push('*');
    }
    if style.bold {
        output.push_str("**");
    }
    if style.code {
        output.push('`');
    }
}

fn render_list(list: &List, output: &mut String, depth: usize) {
    for (index, item) in list.items.iter().enumerate() {
        output.push_str(&"  ".repeat(depth));
        let marker = match list.marker {
            ListMarker::Bullet | ListMarker::None => "- ".to_owned(),
            _ => format!("{}. ", list.start.unwrap_or(1) + index as u64),
        };
        output.push_str(&marker);
        let mut nested = String::new();
        render_blocks(&item.blocks, &mut nested, depth + 1);
        let mut lines = nested.trim().lines();
        output.push_str(lines.next().unwrap_or_default());
        output.push('\n');
        for line in lines {
            output.push_str(&"  ".repeat(depth + 1));
            output.push_str(line);
            output.push('\n');
        }
    }
    output.push('\n');
}

fn render_markdown_table(table: &Table, output: &mut String) {
    if table.columns == 0 {
        return;
    }
    let rows = table.rows.max(1);
    for row in 0..rows {
        output.push('|');
        for column in 0..table.columns {
            output.push(' ');
            output.push_str(&escape_table_cell(&cell_text(table, row, column)));
            output.push_str(" |");
        }
        output.push('\n');
        if row + 1 == table.header_rows.max(1) {
            output.push('|');
            for _ in 0..table.columns {
                output.push_str(" --- |");
            }
            output.push('\n');
        }
    }
    output.push('\n');
}

fn render_html_table(table: &Table, output: &mut String) {
    output.push_str("<table>\n");
    for row in 0..table.rows {
        output.push_str("  <tr>");
        for column in 0..table.columns {
            let Some(CellSlot::Origin(cell)) =
                table.grid.get(row).and_then(|items| items.get(column))
            else {
                continue;
            };
            let tag = if row < table.header_rows { "th" } else { "td" };
            output.push('<');
            output.push_str(tag);
            if cell.row_span > 1 {
                output.push_str(&format!(" rowspan=\"{}\"", cell.row_span));
            }
            if cell.column_span > 1 {
                output.push_str(&format!(" colspan=\"{}\"", cell.column_span));
            }
            output.push('>');
            output.push_str(&escape_html(&plain_cell_text(cell)));
            output.push_str("</");
            output.push_str(tag);
            output.push('>');
        }
        output.push_str("</tr>\n");
    }
    output.push_str("</table>\n\n");
}

fn cell_text(table: &Table, row: usize, column: usize) -> String {
    match table.grid.get(row).and_then(|items| items.get(column)) {
        Some(CellSlot::Origin(cell)) => plain_cell_text(cell),
        _ => String::new(),
    }
}

fn plain_cell_text(cell: &Cell) -> String {
    let mut output = String::new();
    render_blocks(&cell.blocks, &mut output, 0);
    output.trim().replace('\n', " ")
}

fn escape_markdown(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('*', "\\*")
        .replace('_', "\\_")
}

fn escape_table_cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', "<br>")
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CellValueKind, DocumentFormat, DocumentUnit, TableKind};

    #[test]
    fn renders_regular_table_as_gfm() {
        let mut document = CanonicalDocument::new(DocumentFormat::Csv);
        let mut unit = DocumentUnit::new(UnitKind::Sheet, 0, Some("Data".to_owned()));
        unit.blocks.push(Block::Table {
            table: Table {
                kind: TableKind::Data,
                rows: 1,
                columns: 1,
                header_rows: 0,
                grid: vec![vec![CellSlot::Origin(Cell::text(
                    "a|b",
                    CellValueKind::Text,
                ))]],
                caption: None,
            },
        });
        document.units.push(unit);
        let value = markdown(&document);
        assert!(value.contains("a\\|b"));
        assert!(value.contains("| --- |"));
    }
}
