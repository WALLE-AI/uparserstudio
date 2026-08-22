//! The one place Markdown is produced.
//!
//! Format frontends recover source semantics into [`CanonicalDocument`] and
//! never emit Markdown themselves, so escaping, emphasis-marker placement,
//! list indentation and table degradation are decided once here rather than
//! re-derived (and re-broken) per format.

use crate::{
    Block, CanonicalDocument, Cell, CellSlot, FormulaSource, Inline, LinkTarget, List, ListItem,
    ListMarker, NoteKind, Style, Table, UnitKind,
};
use std::borrow::Cow;

pub fn document_json(document: &CanonicalDocument) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(document)
}

/// Resolves an asset id to the href Markdown should point at.
///
/// An embedded image is identified inside the model by a content-addressed
/// id, which is meaningless as a link target. Once a caller has written the
/// asset out it records the location on [`crate::Asset::path`], and that is
/// what an `![](…)` must reference; the id is only a fallback for a document
/// whose assets were never materialised.
pub(crate) struct AssetLinks<'a> {
    by_id: std::collections::HashMap<&'a str, &'a str>,
}

impl<'a> AssetLinks<'a> {
    fn new(document: &'a CanonicalDocument) -> Self {
        Self {
            by_id: document
                .assets
                .iter()
                .filter_map(|asset| Some((asset.id.as_str(), asset.path.as_deref()?)))
                .collect(),
        }
    }

    /// `None` when the asset was never written anywhere.
    ///
    /// Emitting `![](asset-1f3c…)` in that case produces a link that resolves
    /// to nothing; alt text alone is more useful than a broken image.
    fn href(&self, id: &str) -> Option<&'a str> {
        self.by_id.get(id).copied()
    }
}

pub fn markdown(document: &CanonicalDocument) -> String {
    let links = AssetLinks::new(document);
    let mut output = String::new();
    for (unit_index, unit) in document.units.iter().enumerate() {
        if unit.kind == UnitKind::Slide {
            if unit_index > 0 {
                output.push('\n');
            }
            output.push_str(&format!("<a id=\"slide-{}\"></a>\n\n", unit_index + 1));
        }
        if let Some(label) = unit_label_heading(document, unit_index) {
            if unit_index > 0 && unit.kind != UnitKind::Slide {
                output.push('\n');
            }
            output.push_str("# ");
            output.push_str(&escape_inline_text(&label, false));
            output.push_str("\n\n");
        }
        render_blocks(&unit.blocks, &mut output, &links);
    }
    for note in &document.notes {
        output.push_str("[^");
        output.push_str(&note.id);
        output.push_str("]: ");
        let mut note_text = String::new();
        render_blocks(&note.blocks, &mut note_text, &links);
        output.push_str(note_text.trim());
        if note.kind == NoteKind::Comment {
            output.push_str(" (comment)");
        }
        output.push('\n');
    }
    output.trim_end().to_owned() + "\n"
}

/// Render a single block against its owning document.
///
/// Compatibility layers lower one block at a time; without this they had to
/// build a throwaway [`CanonicalDocument`] per block just to call
/// [`markdown`], which re-ran the whole renderer once per block and lost the
/// real document's asset links along the way.
pub fn block_markdown(document: &CanonicalDocument, block: &Block) -> String {
    let links = AssetLinks::new(document);
    let mut output = String::new();
    render_blocks(std::slice::from_ref(block), &mut output, &links);
    output.trim().to_owned()
}

/// Render a table as an HTML table, preserving `rowspan`/`colspan`.
///
/// GFM pipe tables cannot express a merged cell, so a caller that has a
/// richer target (an HTML-capable IR field, say) should take this instead of
/// the Markdown degradation.
pub fn table_html(document: &CanonicalDocument, table: &Table) -> String {
    let links = AssetLinks::new(document);
    let mut output = String::new();
    render_html_table(table, &mut output, &links);
    output.trim().to_owned()
}

/// The synthetic heading a logical unit contributes, if any.
///
/// A single flowing document (DOCX/ODT/RTF) has no unit name worth printing.
/// Sheets and slides do. Chapters usually open with their own heading, and
/// emitting the label as well produced the same title twice in a row — so a
/// label is suppressed when the unit's first block already states it.
fn unit_label_heading(document: &CanonicalDocument, unit_index: usize) -> Option<String> {
    let unit = document.units.get(unit_index)?;
    let label = unit.label.as_ref()?;
    if unit.kind == UnitKind::Flow && document.units.len() == 1 {
        return None;
    }
    if let Some(Block::Heading { content, .. }) = unit.blocks.first()
        && plain_text(content).trim() == label.trim()
    {
        return None;
    }
    Some(label.clone())
}

fn render_blocks(blocks: &[Block], output: &mut String, links: &AssetLinks<'_>) {
    for block in blocks {
        match block {
            Block::Heading { level, content } => {
                output.push_str(&"#".repeat((*level).clamp(1, 6) as usize));
                output.push(' ');
                // Word's built-in Heading styles carry bold run properties, so
                // faithfully resolving the style cascade makes every heading
                // arrive bold. `# **Title**` is redundant markup, not extra
                // fidelity — the heading level already implies the weight.
                output.push_str(render_inlines(&without_bold(content), false, links).trim());
                output.push_str("\n\n");
            }
            Block::Paragraph { content } => {
                let text = render_inlines(content, true, links);
                // Leading whitespace has no paragraph-level meaning in
                // Markdown — four or more spaces would turn the paragraph
                // into an indented code block.
                let text = text.trim();
                if !text.is_empty() {
                    output.push_str(text);
                    output.push_str("\n\n");
                }
            }
            Block::List { list } => render_list(list, output, links),
            Block::Table { table } if table.has_spans() => render_html_table(table, output, links),
            Block::Table { table } => render_markdown_table(table, output, links),
            Block::BlockQuote { blocks } => {
                let mut nested = String::new();
                render_blocks(blocks, &mut nested, links);
                for line in nested.trim_end().lines() {
                    output.push_str("> ");
                    output.push_str(line);
                    output.push('\n');
                }
                output.push('\n');
            }
            Block::CodeBlock { language, text } => {
                let fence = code_fence(text);
                output.push_str(&fence);
                output.push_str(language.as_deref().unwrap_or_default());
                output.push('\n');
                output.push_str(text);
                if !text.ends_with('\n') {
                    output.push('\n');
                }
                output.push_str(&fence);
                output.push_str("\n\n");
            }
            Block::Figure {
                asset_id,
                alt,
                caption,
            } => {
                let alt = alt.as_deref().unwrap_or_default();
                match asset_id.as_deref().and_then(|id| links.href(id)) {
                    Some(href) => {
                        output.push_str("![");
                        output.push_str(&escape_link_label(alt));
                        output.push_str("](");
                        output.push_str(&escape_url(href));
                        output.push(')');
                    }
                    // Unwritten asset: keep whatever the figure described
                    // rather than emitting a link to nothing.
                    None if !alt.is_empty() => {
                        output.push_str(&escape_inline_text(alt, true));
                    }
                    None if caption.is_empty() => continue,
                    None => {}
                }
                if !caption.is_empty() {
                    output.push(' ');
                    output.push_str(render_inlines(caption, false, links).trim());
                }
                output.push_str("\n\n");
            }
            Block::Rule => output.push_str("---\n\n"),
        }
    }
}

/// A fence long enough to survive backtick runs inside the code itself.
fn code_fence(text: &str) -> String {
    let longest = text
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    "`".repeat(longest.max(2) + 1)
}

// ---------------------------------------------------------------------------
// Inline rendering
// ---------------------------------------------------------------------------

/// A styled text run, flattened out of the inline tree so adjacent runs that
/// share a style can be merged before any marker is written.
enum Piece {
    Text { text: String, style: Style },
    Verbatim(String),
    LineBreak,
}

fn render_inlines(inlines: &[Inline], line_start: bool, links: &AssetLinks<'_>) -> String {
    // Single unstyled run is by far the most common shape (every spreadsheet
    // cell, most paragraphs) and needs neither the piece vector nor the merge
    // pass.
    if let [Inline::Text { text, style }] = inlines
        && *style == Style::default()
    {
        let mut output = String::new();
        push_styled(text, style, line_start, &mut output);
        return output;
    }

    let mut pieces = Vec::with_capacity(inlines.len());
    flatten_inlines(inlines, &mut pieces, links);
    merge_adjacent_runs(&mut pieces);

    let mut output = String::new();
    let mut at_line_start = line_start;
    for piece in pieces {
        match piece {
            Piece::Text { text, style } => {
                push_styled(&text, &style, at_line_start, &mut output);
            }
            Piece::Verbatim(value) => output.push_str(&value),
            // Two trailing spaces are the GFM hard line break.
            Piece::LineBreak => output.push_str("  \n"),
        }
        at_line_start = output.ends_with('\n');
    }
    output
}

fn flatten_inlines(inlines: &[Inline], pieces: &mut Vec<Piece>, links: &AssetLinks<'_>) {
    for inline in inlines {
        match inline {
            Inline::Text { text, style } => pieces.push(Piece::Text {
                text: text.clone(),
                style: style.clone(),
            }),
            Inline::Link { target, content } => {
                let label = render_inlines(content, false, links);
                let target = match target {
                    LinkTarget::External(value) => escape_url(value),
                    LinkTarget::Anchor(value) => format!("#{}", escape_url(value)),
                };
                pieces.push(Piece::Verbatim(format!("[{label}]({target})")));
            }
            Inline::Image { source, alt } => {
                let target = match source {
                    crate::ImageSource::Asset(value) => links.href(value),
                    crate::ImageSource::External(value) => Some(value.as_str()),
                };
                let alt = alt.as_deref().unwrap_or_default();
                pieces.push(Piece::Verbatim(match target {
                    Some(target) => {
                        format!("![{}]({})", escape_link_label(alt), escape_url(target))
                    }
                    // See `AssetLinks::href`: no destination, so the alt text
                    // stands in for the image.
                    None => escape_inline_text(alt, false).into_owned(),
                }));
            }
            Inline::Anchor { id } => pieces.push(Piece::Verbatim(format!(
                "<a id=\"{}\"></a>",
                escape_html(id)
            ))),
            Inline::NoteRef { id } => pieces.push(Piece::Verbatim(format!("[^{id}]"))),
            Inline::LineBreak => pieces.push(Piece::LineBreak),
            Inline::Formula { source, display } => {
                let value = match source {
                    FormulaSource::Latex(value) => format!("${value}$"),
                    _ => display.clone().unwrap_or_default(),
                };
                pieces.push(Piece::Verbatim(value));
            }
        }
    }
}

/// Producers split a single formatted phrase across many runs (one per font
/// change, spell-check region, or rsid). Emitting a marker pair per run yields
/// `**a****b**`, which renders as literal asterisks.
fn merge_adjacent_runs(pieces: &mut Vec<Piece>) {
    if pieces.len() < 2 {
        return;
    }
    let mut merged: Vec<Piece> = Vec::with_capacity(pieces.len());
    for piece in pieces.drain(..) {
        match (merged.last_mut(), piece) {
            (
                Some(Piece::Text {
                    text: previous,
                    style: previous_style,
                }),
                Piece::Text { text, style },
            ) if *previous_style == style => previous.push_str(&text),
            (_, piece) => merged.push(piece),
        }
    }
    *pieces = merged;
}

/// Write one styled run, keeping the emphasis markers tight against the text.
///
/// `** bold **` and `**\nbold**` are not emphasis in GFM — a marker must sit
/// directly against a non-space character. Leading and trailing whitespace is
/// therefore hoisted outside the markers, and a run that is *only* whitespace
/// carries no markers at all.
fn push_styled(text: &str, style: &Style, line_start: bool, output: &mut String) {
    if text.is_empty() {
        return;
    }
    let core = text.trim_matches(|character: char| character.is_whitespace());
    if core.is_empty() {
        output.push_str(text);
        return;
    }
    let leading = &text[..text.len() - text.trim_start().len()];
    let trailing = &text[text.trim_end().len()..];

    output.push_str(leading);
    let at_line_start = line_start && leading.is_empty();
    let (open, close) = style_markers(style);
    output.push_str(&open);
    output.push_str(&escape_inline_text(core, at_line_start && open.is_empty()));
    output.push_str(&close);
    output.push_str(trailing);
}

fn style_markers(style: &Style) -> (String, String) {
    // `code` wins over emphasis: Markdown does not apply emphasis markup
    // inside a code span, so nesting them would emit literal asterisks.
    if style.code {
        return ("`".to_owned(), "`".to_owned());
    }
    let mut open = String::new();
    let mut close = String::new();
    if style.bold {
        open.push_str("**");
        close.insert_str(0, "**");
    }
    if style.italic {
        open.push('*');
        close.insert(0, '*');
    }
    if style.strike {
        open.push_str("~~");
        close.insert_str(0, "~~");
    }
    (open, close)
}

// ---------------------------------------------------------------------------
// Lists
// ---------------------------------------------------------------------------

fn render_list(list: &List, output: &mut String, links: &AssetLinks<'_>) {
    for (index, item) in list.items.iter().enumerate() {
        let marker = item_marker(list, index);
        render_list_item(item, &marker, output, links);
    }
    output.push('\n');
}

/// GFM has exactly two list forms, bullet and decimal. Alphabetic and Roman
/// sequences are preserved as literal label text on a bullet item rather than
/// silently renumbered as `1.`, which would erase both the sequence type and
/// the position within it.
fn item_marker(list: &List, index: usize) -> String {
    let ordinal = list.start.unwrap_or(1).saturating_add(index as u64);
    match list.marker {
        ListMarker::Bullet | ListMarker::None => "- ".to_owned(),
        ListMarker::Decimal => format!("{ordinal}. "),
        ListMarker::LowerAlpha => format!("- {}. ", alphabetic_label(ordinal, false)),
        ListMarker::UpperAlpha => format!("- {}. ", alphabetic_label(ordinal, true)),
        ListMarker::LowerRoman => format!("- {}. ", roman_label(ordinal, false)),
        ListMarker::UpperRoman => format!("- {}. ", roman_label(ordinal, true)),
    }
}

/// Indent an item's whole body under its marker so nested lists, paragraphs
/// and tables stay inside the item instead of terminating it.
fn render_list_item(item: &ListItem, marker: &str, output: &mut String, links: &AssetLinks<'_>) {
    let mut body = String::new();
    render_blocks(&item.blocks, &mut body, links);
    let body = body.trim_end();
    let padding = " ".repeat(marker.chars().count());

    if body.is_empty() {
        output.push_str(marker.trim_end());
        output.push('\n');
        return;
    }
    for (line_index, line) in body.lines().enumerate() {
        if line_index == 0 {
            output.push_str(marker);
        } else if !line.is_empty() {
            output.push_str(&padding);
        }
        output.push_str(line);
        output.push('\n');
    }
}

fn alphabetic_label(ordinal: u64, upper: bool) -> String {
    if ordinal == 0 {
        return String::new();
    }
    let mut value = ordinal;
    let mut label = Vec::new();
    while value > 0 {
        let remainder = ((value - 1) % 26) as u8;
        label.push(if upper {
            b'A' + remainder
        } else {
            b'a' + remainder
        });
        value = (value - 1) / 26;
    }
    label.reverse();
    String::from_utf8(label).unwrap_or_default()
}

fn roman_label(ordinal: u64, upper: bool) -> String {
    const TABLE: [(u64, &str); 13] = [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    let mut value = ordinal;
    let mut label = String::new();
    for (weight, numeral) in TABLE {
        while value >= weight {
            label.push_str(numeral);
            value -= weight;
        }
    }
    if upper { label.to_uppercase() } else { label }
}

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

fn render_markdown_table(table: &Table, output: &mut String, links: &AssetLinks<'_>) {
    if table.columns == 0 {
        return;
    }
    // GFM requires a header row. A table that declares none gets an empty one
    // rather than having its first data row promoted, which would delete that
    // row's content from the rendered body.
    let header_rows = table.header_rows.min(table.rows);
    if header_rows == 0 {
        output.push('|');
        for _ in 0..table.columns {
            output.push_str("  |");
        }
        output.push('\n');
        push_separator(table.columns, output);
    }
    for row in 0..table.rows {
        output.push('|');
        for column in 0..table.columns {
            output.push(' ');
            output.push_str(&escape_table_cell(&cell_text(table, row, column, links)));
            output.push_str(" |");
        }
        output.push('\n');
        if header_rows > 0 && row + 1 == header_rows {
            push_separator(table.columns, output);
        }
    }
    output.push('\n');
}

fn push_separator(columns: usize, output: &mut String) {
    output.push('|');
    for _ in 0..columns {
        output.push_str(" --- |");
    }
    output.push('\n');
}

fn render_html_table(table: &Table, output: &mut String, links: &AssetLinks<'_>) {
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
            output.push_str(&escape_html(&plain_cell_text(cell, links)));
            output.push_str("</");
            output.push_str(tag);
            output.push('>');
        }
        output.push_str("</tr>\n");
    }
    output.push_str("</table>\n\n");
}

fn cell_text(table: &Table, row: usize, column: usize, links: &AssetLinks<'_>) -> String {
    match table.grid.get(row).and_then(|items| items.get(column)) {
        Some(CellSlot::Origin(cell)) => plain_cell_text(cell, links),
        _ => String::new(),
    }
}

fn plain_cell_text(cell: &Cell, links: &AssetLinks<'_>) -> String {
    // A spreadsheet cell is one unstyled paragraph; short-circuiting that
    // shape avoids four allocations per cell, which dominates rendering for
    // sheets with hundreds of thousands of them.
    if let [Block::Paragraph { content }] = cell.blocks.as_slice()
        && let [Inline::Text { text, style }] = content.as_slice()
        && *style == Style::default()
    {
        return escape_inline_text(text.trim(), false).into_owned();
    }
    let mut output = String::new();
    render_blocks(&cell.blocks, &mut output, links);
    output
        .trim()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn without_bold(inlines: &[Inline]) -> Vec<Inline> {
    inlines
        .iter()
        .map(|inline| match inline {
            Inline::Text { text, style } => Inline::Text {
                text: text.clone(),
                style: Style {
                    bold: false,
                    ..style.clone()
                },
            },
            Inline::Link { target, content } => Inline::Link {
                target: target.clone(),
                content: without_bold(content),
            },
            other => other.clone(),
        })
        .collect()
}

fn plain_text(inlines: &[Inline]) -> String {
    let mut output = String::new();
    for inline in inlines {
        match inline {
            Inline::Text { text, .. } => output.push_str(text),
            Inline::Link { content, .. } => output.push_str(&plain_text(content)),
            Inline::LineBreak => output.push(' '),
            Inline::Formula { display, .. } => {
                output.push_str(display.as_deref().unwrap_or_default())
            }
            _ => {}
        }
    }
    output
}

// ---------------------------------------------------------------------------
// Escaping
// ---------------------------------------------------------------------------

/// Escape body text.
///
/// Deliberately narrow: escaping every Markdown-adjacent byte (the previous
/// blanket `\ * _` replacement did) corrupts ordinary prose — `snake_case`
/// became `snake\_case` — for no rendering benefit, because GFM only treats
/// `_` as emphasis at a word boundary. Only constructs that would actually
/// change the parse are escaped.
fn escape_inline_text(text: &str, line_start: bool) -> Cow<'_, str> {
    // Overwhelmingly the common case — spreadsheet cells, plain prose — needs
    // no escaping at all. Scanning bytes first keeps that path allocation-free
    // instead of rebuilding every string character by character.
    if !text.bytes().any(needs_escape) {
        return if line_start {
            escape_line_start(text)
        } else {
            Cow::Borrowed(text)
        };
    }

    let mut output = String::with_capacity(text.len() + 8);
    let mut previous = None;
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        let next = characters.peek().copied();
        match character {
            '\\' | '`' | '*' | '[' | ']' => {
                output.push('\\');
                output.push(character);
            }
            // `_` is emphasis only at a word boundary; intra-word underscores
            // are literal in GFM and must not be escaped.
            '_' if is_word_boundary(previous) || is_word_boundary(next) => {
                output.push('\\');
                output.push('_');
            }
            '<' => output.push_str("&lt;"),
            '&' => output.push_str("&amp;"),
            _ => output.push(character),
        }
        previous = Some(character);
    }
    if line_start {
        Cow::Owned(escape_line_start(&output).into_owned())
    } else {
        Cow::Owned(output)
    }
}

const fn needs_escape(byte: u8) -> bool {
    matches!(byte, b'\\' | b'`' | b'*' | b'[' | b']' | b'_' | b'<' | b'&')
}

fn is_word_boundary(character: Option<char>) -> bool {
    character.is_none_or(|character| !character.is_alphanumeric())
}

/// Escape constructs that only have block meaning at the start of a line.
fn escape_line_start(text: &str) -> Cow<'_, str> {
    let trimmed = text.trim_start();
    let indent_len = text.len() - trimmed.len();
    let (indent, rest) = text.split_at(indent_len);
    let first = rest.chars().next();
    let is_block_marker = matches!(first, Some('#' | '>' | '-' | '+' | '=' | '|'))
        || first.is_some_and(|first| first.is_ascii_digit());
    if !is_block_marker {
        return Cow::Borrowed(text);
    }
    let escaped = match first {
        Some(first @ ('#' | '>' | '-' | '+' | '=' | '|')) => {
            format!("\\{first}{}", &rest[first.len_utf8()..])
        }
        Some(first) if first.is_ascii_digit() => {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            match rest[digits.len()..].chars().next() {
                Some(marker @ ('.' | ')')) => format!(
                    "{digits}\\{marker}{}",
                    &rest[digits.len() + marker.len_utf8()..]
                ),
                _ => rest.to_owned(),
            }
        }
        _ => rest.to_owned(),
    };
    Cow::Owned(format!("{indent}{escaped}"))
}

/// A link label may not contain unescaped brackets, but emphasis inside it is
/// legal and already rendered, so only the bracket pair is touched.
fn escape_link_label(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

/// Angle brackets, parentheses and whitespace terminate an inline link
/// destination.
fn escape_url(url: &str) -> String {
    if url.contains(' ') || url.contains('(') || url.contains(')') {
        format!("<{}>", url.replace('<', "%3C").replace('>', "%3E"))
    } else {
        url.to_owned()
    }
}

fn escape_table_cell(text: &str) -> Cow<'_, str> {
    if !text.bytes().any(|byte| matches!(byte, b'|' | b'\n')) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(text.replace('|', "\\|").replace('\n', "<br>"))
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

    fn styled(text: &str, style: Style) -> Inline {
        Inline::Text {
            text: text.to_owned(),
            style,
        }
    }

    fn bold() -> Style {
        Style {
            bold: true,
            ..Style::default()
        }
    }

    fn flow(blocks: Vec<Block>) -> CanonicalDocument {
        let mut document = CanonicalDocument::new(DocumentFormat::Docx);
        let mut unit = DocumentUnit::new(UnitKind::Flow, 0, None);
        unit.blocks = blocks;
        document.units.push(unit);
        document
    }

    #[test]
    fn canonical_document_empty_state_follows_unit_blocks() {
        let mut document = CanonicalDocument::new(DocumentFormat::Docx);
        assert!(document.is_empty());
        document
            .units
            .push(DocumentUnit::new(UnitKind::Flow, 0, None));
        assert!(document.is_empty());
        document.units[0].blocks.push(Block::paragraph("content"));
        assert!(!document.is_empty());
    }

    #[test]
    fn renders_regular_table_as_gfm() {
        let mut document = CanonicalDocument::new(DocumentFormat::Csv);
        let mut unit = DocumentUnit::new(UnitKind::Sheet, 0, Some("Data".to_owned()));
        unit.blocks.push(Block::Table {
            table: Table {
                kind: TableKind::Data,
                rows: 1,
                columns: 1,
                header_rows: 1,
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

    #[test]
    fn headerless_table_keeps_its_first_row_as_data() {
        let document = flow(vec![Block::Table {
            table: Table {
                kind: TableKind::Data,
                rows: 1,
                columns: 1,
                header_rows: 0,
                grid: vec![vec![CellSlot::Origin(Cell::text(
                    "only row",
                    CellValueKind::Text,
                ))]],
                caption: None,
            },
        }]);
        let value = markdown(&document);
        let lines: Vec<&str> = value.lines().collect();
        // Empty header, separator, then the data row — the row is not consumed
        // as a header.
        assert_eq!(lines[0].trim(), "|  |");
        assert_eq!(lines[1].trim(), "| --- |");
        assert!(lines[2].contains("only row"));
    }

    #[test]
    fn emphasis_markers_stay_tight_against_the_text() {
        // A run that carries its own trailing whitespace previously produced
        // `**bold **`, which GFM renders as literal asterisks.
        let document = flow(vec![Block::Paragraph {
            content: vec![
                Inline::text("Plain "),
                styled("bold ", bold()),
                Inline::text("tail"),
            ],
        }]);
        assert!(markdown(&document).contains("Plain **bold** tail"));
    }

    #[test]
    fn whitespace_only_styled_run_emits_no_markers() {
        let document = flow(vec![Block::Paragraph {
            content: vec![Inline::text("a"), styled(" ", bold()), Inline::text("b")],
        }]);
        let value = markdown(&document);
        assert!(value.contains("a b"), "{value}");
        assert!(!value.contains("**"), "{value}");
    }

    #[test]
    fn adjacent_runs_with_the_same_style_share_one_marker_pair() {
        let document = flow(vec![Block::Paragraph {
            content: vec![styled("bo", bold()), styled("ld", bold())],
        }]);
        let value = markdown(&document);
        assert!(value.contains("**bold**"), "{value}");
        assert!(!value.contains("****"), "{value}");
    }

    #[test]
    fn intra_word_underscores_are_not_escaped() {
        let document = flow(vec![Block::Paragraph {
            content: vec![Inline::text("call snake_case_name here")],
        }]);
        let value = markdown(&document);
        assert!(value.contains("snake_case_name"), "{value}");
    }

    #[test]
    fn line_leading_block_markers_are_escaped() {
        let document = flow(vec![Block::Paragraph {
            content: vec![Inline::text("# not a heading")],
        }]);
        assert!(markdown(&document).contains("\\# not a heading"));
    }

    #[test]
    fn nested_list_is_indented_under_its_parent_item() {
        let nested = List {
            marker: ListMarker::Bullet,
            start: None,
            items: vec![ListItem {
                blocks: vec![Block::paragraph("inner")],
            }],
        };
        let document = flow(vec![Block::List {
            list: List {
                marker: ListMarker::Decimal,
                start: Some(1),
                items: vec![ListItem {
                    blocks: vec![Block::paragraph("outer"), Block::List { list: nested }],
                }],
            },
        }]);
        let value = markdown(&document);
        assert!(value.contains("1. outer"), "{value}");
        assert!(value.contains("\n   - inner"), "{value}");
    }

    #[test]
    fn ordered_list_numbering_follows_its_start_value() {
        let document = flow(vec![Block::List {
            list: List {
                marker: ListMarker::Decimal,
                start: Some(4),
                items: vec![
                    ListItem {
                        blocks: vec![Block::paragraph("four")],
                    },
                    ListItem {
                        blocks: vec![Block::paragraph("five")],
                    },
                ],
            },
        }]);
        let value = markdown(&document);
        assert!(value.contains("4. four"), "{value}");
        assert!(value.contains("5. five"), "{value}");
    }

    #[test]
    fn alphabetic_and_roman_sequences_keep_their_labels() {
        let document = flow(vec![
            Block::List {
                list: List {
                    marker: ListMarker::LowerAlpha,
                    start: Some(1),
                    items: vec![ListItem {
                        blocks: vec![Block::paragraph("alpha")],
                    }],
                },
            },
            Block::List {
                list: List {
                    marker: ListMarker::UpperRoman,
                    start: Some(4),
                    items: vec![ListItem {
                        blocks: vec![Block::paragraph("roman")],
                    }],
                },
            },
        ]);
        let value = markdown(&document);
        assert!(value.contains("- a. alpha"), "{value}");
        assert!(value.contains("- IV. roman"), "{value}");
    }

    #[test]
    fn chapter_label_is_not_repeated_when_the_first_block_already_states_it() {
        let mut document = CanonicalDocument::new(DocumentFormat::Epub);
        for (index, title) in ["Chapter One", "Chapter Two"].into_iter().enumerate() {
            let mut unit = DocumentUnit::new(UnitKind::Chapter, index, Some(title.to_owned()));
            unit.blocks.push(Block::Heading {
                level: 1,
                content: vec![Inline::text(title)],
            });
            unit.blocks.push(Block::paragraph("body"));
            document.units.push(unit);
        }
        let value = markdown(&document);
        assert_eq!(value.matches("# Chapter One").count(), 1, "{value}");
        assert_eq!(value.matches("# Chapter Two").count(), 1, "{value}");
    }

    #[test]
    fn sheet_label_is_kept_because_it_appears_nowhere_else() {
        let mut document = CanonicalDocument::new(DocumentFormat::Excel);
        let mut unit = DocumentUnit::new(UnitKind::Sheet, 0, Some("Q1".to_owned()));
        unit.blocks.push(Block::paragraph("body"));
        document.units.push(unit);
        assert!(markdown(&document).contains("# Q1"));
    }

    #[test]
    fn slides_receive_stable_anchors_without_changing_model_blocks() {
        let mut document = CanonicalDocument::new(DocumentFormat::Pptx);
        for (index, title) in ["Custom title", "Second title"].into_iter().enumerate() {
            let mut unit = DocumentUnit::new(UnitKind::Slide, index, Some(title.to_owned()));
            unit.blocks.push(Block::Heading {
                level: 1,
                content: vec![Inline::text(title)],
            });
            document.units.push(unit);
        }

        let value = markdown(&document);
        assert!(value.starts_with("<a id=\"slide-1\"></a>\n\n# Custom title"));
        assert!(value.contains("<a id=\"slide-2\"></a>\n\n# Second title"));
        assert_eq!(document.units[0].blocks.len(), 1);
    }

    #[test]
    fn code_fence_survives_backticks_in_the_code() {
        let document = flow(vec![Block::CodeBlock {
            language: None,
            text: "let a = ``x``;".to_owned(),
        }]);
        let value = markdown(&document);
        assert!(value.contains("```\n"), "{value}");
    }
}
