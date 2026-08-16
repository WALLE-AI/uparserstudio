use crate::{
    Asset, Block, CanonicalDocument, Cell, CellSlot, CellValueKind, DocumentError, DocumentFormat,
    DocumentUnit, Inline, LinkTarget, List, ListItem, ListMarker, Note, NoteKind, ParseOptions,
    ParseWarning, Style, Table, TableKind, UnitKind, WarningCode,
};
use encoding_rs::{Encoding, SHIFT_JIS, WINDOWS_1250, WINDOWS_1251, WINDOWS_1252};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Destination {
    Normal,
    Skip,
    FieldInstruction,
    FieldResult,
    Footnote,
    Bookmark,
    Picture,
}

#[derive(Clone)]
struct State {
    style: Style,
    destination: Destination,
    destination_origin: usize,
    ignorable: bool,
    codepage: u16,
    unicode_skip: usize,
    /// List membership comes from `\ls` (a `\listoverridetable` index) alone.
    /// `\ilvl` only names a level *within* a list and is emitted by producers
    /// on ordinary styled paragraphs too — treating it as membership turned
    /// every heading and body paragraph into a numbered item.
    list_id: Option<i64>,
    list_level: u8,
}

impl Default for State {
    fn default() -> Self {
        Self {
            style: Style::default(),
            destination: Destination::Normal,
            destination_origin: 0,
            ignorable: false,
            codepage: 1252,
            unicode_skip: 1,
            list_id: None,
            list_level: 0,
        }
    }
}

struct Paragraph {
    content: Vec<Inline>,
    heading: Option<u8>,
    list_level: Option<u8>,
}

impl Paragraph {
    fn new(state: &State) -> Self {
        Self {
            content: Vec::new(),
            heading: None,
            list_level: state.list_id.map(|_| state.list_level),
        }
    }
}

#[derive(Default)]
struct TableBuilder {
    rows: Vec<Vec<Cell>>,
    row: Vec<Cell>,
    cell_blocks: Vec<Block>,
    active: bool,
}

struct Parser<'a> {
    options: &'a ParseOptions,
    document: CanonicalDocument,
    unit: DocumentUnit,
    state: State,
    stack: Vec<State>,
    paragraph: Paragraph,
    table: TableBuilder,
    captures: String,
    picture: Vec<u8>,
    picture_type: &'static str,
    pending_hex: Vec<u8>,
    pending_unicode: Vec<u16>,
    skip_fallback: usize,
    pending_field_target: Option<String>,
    tokens: usize,
    text_bytes: usize,
    note_id: usize,
}

pub(crate) fn parse(
    bytes: &[u8],
    options: &ParseOptions,
) -> Result<CanonicalDocument, DocumentError> {
    if !bytes.starts_with(b"{\\rtf") {
        return Err(DocumentError::Malformed {
            part: None,
            detail: "RTF input does not start with an RTF header".to_owned(),
        });
    }
    let state = State::default();
    let parser = Parser {
        options,
        document: CanonicalDocument::new(DocumentFormat::Rtf),
        unit: DocumentUnit::new(UnitKind::Flow, 0, None),
        paragraph: Paragraph::new(&state),
        state,
        stack: Vec::new(),
        table: TableBuilder::default(),
        captures: String::new(),
        picture: Vec::new(),
        picture_type: "application/octet-stream",
        pending_hex: Vec::new(),
        pending_unicode: Vec::new(),
        skip_fallback: 0,
        pending_field_target: None,
        tokens: 0,
        text_bytes: 0,
        note_id: 0,
    };
    parser.run(bytes)
}

impl Parser<'_> {
    fn run(mut self, bytes: &[u8]) -> Result<CanonicalDocument, DocumentError> {
        self.document.metadata.variant = Some("rtf".to_owned());
        let mut cursor = 0usize;
        while cursor < bytes.len() {
            self.token()?;
            match bytes[cursor] {
                b'{' => {
                    self.flush_text()?;
                    if self.stack.len() >= self.options.limits.max_xml_depth {
                        return Err(self.limit("max_xml_depth", "RTF group nesting is too deep"));
                    }
                    self.stack.push(self.state.clone());
                    cursor += 1;
                }
                b'}' => {
                    self.flush_text()?;
                    self.close_destination()?;
                    let Some(state) = self.stack.pop() else {
                        self.document.warnings.push(ParseWarning {
                            code: WarningCode::TruncatedContent,
                            part: None,
                            message: "RTF contains an unmatched closing group".to_owned(),
                        });
                        cursor += 1;
                        continue;
                    };
                    self.state = state;
                    cursor += 1;
                }
                b'\\' => {
                    cursor += 1;
                    cursor = self.control(bytes, cursor)?;
                }
                _ => {
                    let start = cursor;
                    while cursor < bytes.len() && !matches!(bytes[cursor], b'{' | b'}' | b'\\') {
                        cursor += 1;
                    }
                    self.plain_text(&bytes[start..cursor])?;
                }
            }
        }
        self.flush_text()?;
        if !self.stack.is_empty() {
            self.document.warnings.push(ParseWarning {
                code: WarningCode::TruncatedContent,
                part: None,
                message: format!("RTF ended with {} unclosed groups", self.stack.len()),
            });
        }
        self.finish_paragraph();
        self.finish_table();
        self.document.units.push(self.unit);
        Ok(self.document)
    }

    fn token(&mut self) -> Result<(), DocumentError> {
        self.tokens += 1;
        if self.tokens > self.options.limits.max_xml_nodes {
            return Err(self.limit("max_xml_nodes", "RTF token budget exceeded"));
        }
        Ok(())
    }

    fn control(&mut self, bytes: &[u8], mut cursor: usize) -> Result<usize, DocumentError> {
        if cursor >= bytes.len() {
            return Ok(cursor);
        }
        match bytes[cursor] {
            b'\\' | b'{' | b'}' => {
                self.flush_text()?;
                self.emit_text(&[bytes[cursor]])?;
                return Ok(cursor + 1);
            }
            b'\'' if cursor + 2 < bytes.len() => {
                if let (Some(high), Some(low)) = (hex(bytes[cursor + 1]), hex(bytes[cursor + 2])) {
                    if self.skip_fallback > 0 {
                        self.skip_fallback -= 1;
                    } else if self.state.destination == Destination::Picture {
                        self.picture.push((high << 4) | low);
                        self.check_picture_budget()?;
                    } else {
                        self.pending_hex.push((high << 4) | low);
                    }
                    return Ok(cursor + 3);
                }
            }
            b'*' => {
                self.state.ignorable = true;
                return Ok(cursor + 1);
            }
            b'~' => {
                self.emit_str("\u{a0}")?;
                return Ok(cursor + 1);
            }
            b'_' => {
                self.emit_str("-")?;
                return Ok(cursor + 1);
            }
            b'-' => return Ok(cursor + 1),
            _ => {}
        }
        if !bytes[cursor].is_ascii_alphabetic() {
            return Ok(cursor + 1);
        }
        let start = cursor;
        while cursor < bytes.len() && bytes[cursor].is_ascii_alphabetic() {
            cursor += 1;
        }
        let word = std::str::from_utf8(&bytes[start..cursor]).unwrap_or_default();
        let mut sign = 1i64;
        if cursor < bytes.len() && bytes[cursor] == b'-' {
            sign = -1;
            cursor += 1;
        }
        let number_start = cursor;
        while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
            cursor += 1;
        }
        let parameter = (number_start != cursor)
            .then(|| {
                std::str::from_utf8(&bytes[number_start..cursor])
                    .ok()
                    .and_then(|value| value.parse::<i64>().ok())
                    .map(|value| value * sign)
            })
            .flatten();
        if cursor < bytes.len() && bytes[cursor] == b' ' {
            cursor += 1;
        }
        if word == "u" {
            self.flush_hex()?;
        } else {
            self.flush_text()?;
        }
        if word == "bin" {
            let length = parameter.unwrap_or(0).max(0) as usize;
            if length > self.options.limits.max_asset_bytes
                || cursor.saturating_add(length) > bytes.len()
            {
                return Err(self.limit(
                    "max_asset_bytes",
                    "RTF binary payload is too large or truncated",
                ));
            }
            if self.state.destination == Destination::Picture {
                self.picture
                    .extend_from_slice(&bytes[cursor..cursor + length]);
                self.check_picture_budget()?;
            }
            return Ok(cursor + length);
        }
        self.apply_control(word, parameter)?;
        Ok(cursor)
    }

    fn apply_control(&mut self, word: &str, parameter: Option<i64>) -> Result<(), DocumentError> {
        // Control words inside a skipped destination describe that table's own
        // entries, not the document body. A stylesheet entry such as
        // `{\s1 …\ilvl0\outlinelevel0… heading 1;}` would otherwise mark the
        // *next real paragraph* as a heading and a list item — which is
        // exactly what turned every RTF paragraph into a numbered item.
        if self.state.destination == Destination::Skip {
            return Ok(());
        }
        let enabled = parameter != Some(0);
        match word {
            "ansi" => self.state.codepage = 1252,
            "ansicpg" => self.state.codepage = parameter.unwrap_or(1252).max(0) as u16,
            "uc" => self.state.unicode_skip = parameter.unwrap_or(1).max(0) as usize,
            "u" => {
                let value = parameter.unwrap_or(0) as i16 as u16;
                self.pending_unicode.push(value);
                self.skip_fallback = self.state.unicode_skip;
            }
            "b" => self.state.style.bold = enabled,
            "i" => self.state.style.italic = enabled,
            "ul" | "ulw" => self.state.style.underline = enabled,
            "ulnone" => self.state.style.underline = false,
            "strike" => self.state.style.strike = enabled,
            "super" => self.state.style.superscript = Some(true),
            "sub" => self.state.style.superscript = Some(false),
            "nosupersub" => self.state.style.superscript = None,
            "plain" => self.state.style = Style::default(),
            "pard" => {
                self.state.list_id = None;
                self.state.list_level = 0;
                self.paragraph.heading = None;
                self.paragraph.list_level = None;
            }
            "par" => self.finish_paragraph(),
            "line" => self.emit_inline(Inline::LineBreak),
            "tab" => self.emit_str("\t")?,
            "page" => {
                self.finish_paragraph();
                self.unit.blocks.push(Block::Rule);
            }
            "outlinelevel" => {
                self.paragraph.heading = Some((parameter.unwrap_or(0) + 1).clamp(1, 6) as u8)
            }
            "ls" => self.state.list_id = Some(parameter.unwrap_or(0)),
            "ilvl" | "listlevel" => {
                self.state.list_level = parameter.unwrap_or(0).clamp(0, 8) as u8
            }
            "trowd" => {
                self.finish_paragraph();
                self.table.active = true;
            }
            "cell" => self.finish_cell(),
            "row" => self.finish_row(),
            "field" => {}
            "fldinst" => self.set_destination(Destination::FieldInstruction),
            "fldrslt" => self.set_destination(Destination::FieldResult),
            "footnote" | "endnote" => {
                if !self.options.include_notes {
                    self.set_destination(Destination::Skip);
                    return Ok(());
                }
                self.note_id += 1;
                self.emit_inline(Inline::NoteRef {
                    id: format!("rtf-note-{}", self.note_id),
                });
                self.set_destination(Destination::Footnote);
            }
            "bkmkstart" => self.set_destination(Destination::Bookmark),
            "pict" => self.set_destination(Destination::Picture),
            "pngblip" => self.picture_type = "image/png",
            "jpegblip" => self.picture_type = "image/jpeg",
            "emfblip" => self.picture_type = "image/emf",
            "wmetafile" => self.picture_type = "image/wmf",
            // Running headers and footers repeat on every page, so they are
            // excluded from the body unless the caller asks for them.
            "header" | "footer" | "headerl" | "headerr" | "headerf" | "footerl" | "footerr"
            | "footerf" => {
                if !self.options.include_headers_footers {
                    self.set_destination(Destination::Skip);
                }
            }
            "fonttbl" | "colortbl" | "stylesheet" | "listtable" | "listoverridetable"
            | "listtext" | "pntext" | "info" | "generator" | "xmlnstbl" | "datastore"
            | "themedata" => self.set_destination(Destination::Skip),
            _ if self.state.ignorable => self.set_destination(Destination::Skip),
            _ => {}
        }
        Ok(())
    }

    fn set_destination(&mut self, destination: Destination) {
        self.state.destination = destination;
        self.state.destination_origin = self.stack.len();
        self.captures.clear();
        if destination == Destination::Picture {
            self.picture.clear();
            self.picture_type = "application/octet-stream";
        }
    }

    fn close_destination(&mut self) -> Result<(), DocumentError> {
        if self.state.destination_origin != self.stack.len() {
            return Ok(());
        }
        match self.state.destination {
            Destination::FieldInstruction => {
                self.pending_field_target = parse_hyperlink(&self.captures);
            }
            Destination::FieldResult => {
                if !self.captures.is_empty() {
                    let content = vec![Inline::Text {
                        text: std::mem::take(&mut self.captures),
                        style: self.state.style.clone(),
                    }];
                    if let Some(target) = self.pending_field_target.take() {
                        self.paragraph.content.push(Inline::Link {
                            target: if let Some(anchor) = target.strip_prefix('#') {
                                LinkTarget::Anchor(anchor.to_owned())
                            } else {
                                LinkTarget::External(target)
                            },
                            content,
                        });
                    } else {
                        self.paragraph.content.extend(content);
                    }
                }
            }
            Destination::Footnote => {
                let text = std::mem::take(&mut self.captures);
                if !text.trim().is_empty() {
                    self.document.notes.push(Note {
                        id: format!("rtf-note-{}", self.note_id),
                        kind: NoteKind::Footnote,
                        blocks: vec![Block::paragraph(text.trim())],
                    });
                }
            }
            Destination::Bookmark => {
                let id = std::mem::take(&mut self.captures);
                if !id.trim().is_empty() {
                    self.paragraph.content.push(Inline::Anchor {
                        id: id.trim().to_owned(),
                    });
                }
            }
            Destination::Picture => self.finish_picture(),
            Destination::Normal | Destination::Skip => {}
        }
        Ok(())
    }

    fn plain_text(&mut self, bytes: &[u8]) -> Result<(), DocumentError> {
        if self.state.destination == Destination::Picture {
            for byte in bytes
                .iter()
                .copied()
                .filter(|byte| !byte.is_ascii_whitespace())
            {
                if let Some(value) = hex(byte) {
                    if self.pending_hex.is_empty() {
                        self.pending_hex.push(value);
                    } else {
                        let high = self.pending_hex.pop().unwrap();
                        self.picture.push((high << 4) | value);
                    }
                }
            }
            return self.check_picture_budget();
        }
        if bytes.contains(&b'\n') && bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(());
        }
        // CR and LF carry no meaning in an RTF stream — only `\par` and
        // `\line` break lines. Passing them through put raw newlines inside
        // styled runs, which split emphasis markers across lines
        // (`**\nbold**`) and stopped them rendering as emphasis at all.
        // Stripping at byte level is safe for the multi-byte codepages here:
        // no DBCS trail byte in the supported set is 0x0A or 0x0D.
        let stripped: Vec<u8>;
        let mut bytes = if bytes.contains(&b'\n') || bytes.contains(&b'\r') {
            stripped = bytes
                .iter()
                .copied()
                .filter(|byte| !matches!(byte, b'\n' | b'\r'))
                .collect();
            stripped.as_slice()
        } else {
            bytes
        };
        if self.skip_fallback > 0 {
            let skip = self.skip_fallback.min(bytes.len());
            bytes = &bytes[skip..];
            self.skip_fallback -= skip;
        }
        if !bytes.is_empty() {
            self.flush_text()?;
            let (text, _, _) = encoding(self.state.codepage).decode(bytes);
            self.emit_str(&text)?;
        }
        Ok(())
    }

    fn flush_text(&mut self) -> Result<(), DocumentError> {
        if !self.pending_unicode.is_empty() {
            let text: String = char::decode_utf16(self.pending_unicode.drain(..))
                .map(|value| value.unwrap_or(char::REPLACEMENT_CHARACTER))
                .collect();
            self.emit_str(&text)?;
        }
        self.flush_hex()
    }

    fn flush_hex(&mut self) -> Result<(), DocumentError> {
        if !self.pending_hex.is_empty() && self.state.destination != Destination::Picture {
            let bytes = std::mem::take(&mut self.pending_hex);
            let (text, _, _) = encoding(self.state.codepage).decode(&bytes);
            self.emit_str(&text)?;
        }
        Ok(())
    }

    fn emit_text(&mut self, bytes: &[u8]) -> Result<(), DocumentError> {
        let (text, _, _) = encoding(self.state.codepage).decode(bytes);
        self.emit_str(&text)
    }

    fn emit_str(&mut self, text: &str) -> Result<(), DocumentError> {
        self.text_bytes = self.text_bytes.saturating_add(text.len());
        if self.text_bytes > self.options.limits.max_text_bytes {
            return Err(self.limit(
                "max_text_bytes",
                "RTF decoded text exceeds the configured budget",
            ));
        }
        match self.state.destination {
            Destination::Normal => self.emit_inline(Inline::Text {
                text: text.to_owned(),
                style: self.state.style.clone(),
            }),
            Destination::FieldInstruction
            | Destination::FieldResult
            | Destination::Footnote
            | Destination::Bookmark => self.captures.push_str(text),
            Destination::Skip | Destination::Picture => {}
        }
        Ok(())
    }

    fn emit_inline(&mut self, inline: Inline) {
        if self.state.destination == Destination::Normal {
            if self.paragraph.content.is_empty() {
                self.paragraph.list_level = self.state.list_id.map(|_| self.state.list_level);
            }
            self.paragraph.content.push(inline);
        }
    }

    fn finish_paragraph(&mut self) {
        if self.paragraph.content.is_empty() {
            self.paragraph = Paragraph::new(&self.state);
            return;
        }
        let paragraph = std::mem::replace(&mut self.paragraph, Paragraph::new(&self.state));
        let block = match paragraph.heading {
            Some(level) => Block::Heading {
                level,
                content: paragraph.content,
            },
            None => Block::Paragraph {
                content: paragraph.content,
            },
        };
        if self.table.active {
            self.table.cell_blocks.push(block);
        } else if paragraph.heading.is_some() {
            // A styled heading may also carry list properties from its style
            // definition; the heading is the stronger signal.
            if !self.table.rows.is_empty() {
                self.finish_table();
            }
            self.unit.blocks.push(block);
        } else if paragraph.list_level.is_some() {
            if !self.table.rows.is_empty() {
                self.finish_table();
            }
            self.append_list_item(block, paragraph.list_level.unwrap_or(0));
        } else {
            if !self.table.rows.is_empty() {
                self.finish_table();
            }
            self.unit.blocks.push(block);
        }
    }

    fn append_list_item(&mut self, block: Block, level: u8) {
        push_nested_list_item(&mut self.unit.blocks, level, block);
    }

    fn finish_cell(&mut self) {
        self.finish_paragraph();
        self.table.row.push(Cell {
            row_span: 1,
            column_span: 1,
            value_kind: if self.table.cell_blocks.is_empty() {
                CellValueKind::Empty
            } else {
                CellValueKind::Text
            },
            formula: None,
            blocks: std::mem::take(&mut self.table.cell_blocks),
        });
    }

    fn finish_row(&mut self) {
        self.finish_paragraph();
        if !self.table.cell_blocks.is_empty() {
            self.finish_cell();
        }
        if !self.table.row.is_empty() {
            self.table.rows.push(std::mem::take(&mut self.table.row));
        }
        self.table.active = false;
    }

    fn finish_table(&mut self) {
        if !self.table.row.is_empty() {
            self.table.rows.push(std::mem::take(&mut self.table.row));
        }
        if self.table.rows.is_empty() {
            return;
        }
        let columns = self.table.rows.iter().map(Vec::len).max().unwrap_or(0);
        let rows = self.table.rows.len();
        let grid = self
            .table
            .rows
            .drain(..)
            .map(|mut row| {
                row.resize_with(columns, || Cell::text("", CellValueKind::Empty));
                row.into_iter().map(CellSlot::Origin).collect()
            })
            .collect();
        self.unit.blocks.push(Block::Table {
            table: Table {
                kind: TableKind::Data,
                rows,
                columns,
                header_rows: 0,
                grid,
                caption: None,
            },
        });
    }

    fn finish_picture(&mut self) {
        if self.picture.is_empty() {
            return;
        }
        let bytes = std::mem::take(&mut self.picture);
        let sha256 = format!("{:x}", Sha256::digest(&bytes));
        let id = format!("asset-{}", &sha256[..16]);
        if !self.document.assets.iter().any(|asset| asset.id == id) {
            self.document.assets.push(Asset {
                id: id.clone(),
                media_type: self.picture_type.to_owned(),
                filename: None,
                byte_length: bytes.len(),
                sha256,
                path: None,
                bytes: self.options.include_assets.then_some(bytes),
            });
        }
        self.unit.blocks.push(Block::Figure {
            asset_id: Some(id),
            alt: None,
            caption: Vec::new(),
        });
    }

    fn check_picture_budget(&self) -> Result<(), DocumentError> {
        if self.picture.len() > self.options.limits.max_asset_bytes {
            return Err(self.limit(
                "max_asset_bytes",
                "RTF picture exceeds the configured budget",
            ));
        }
        Ok(())
    }

    fn limit(&self, name: &'static str, detail: impl Into<String>) -> DocumentError {
        DocumentError::ResourceLimit {
            limit: name,
            detail: detail.into(),
        }
    }
}

/// Place a list item at `level`, descending into the enclosing item chain so
/// `\ilvl1` paragraphs nest under their `\ilvl0` parent instead of flattening
/// into one sequence.
fn push_nested_list_item(container: &mut Vec<Block>, level: u8, block: Block) {
    if level > 0
        && let Some(Block::List { list }) = container.last_mut()
    {
        if list.items.is_empty() {
            list.items.push(ListItem { blocks: Vec::new() });
        }
        let inner = &mut list.items.last_mut().unwrap().blocks;
        push_nested_list_item(inner, level - 1, block);
        return;
    }
    let item = ListItem {
        blocks: vec![block],
    };
    if let Some(Block::List { list }) = container.last_mut() {
        list.items.push(item);
        return;
    }
    container.push(Block::List {
        list: List {
            // RTF list *types* live in `\listtable`, which is not parsed yet;
            // items are emitted as a decimal sequence until it is.
            marker: ListMarker::Decimal,
            start: None,
            items: vec![item],
        },
    });
}

fn encoding(codepage: u16) -> &'static Encoding {
    match codepage {
        932 => SHIFT_JIS,
        1250 => WINDOWS_1250,
        1251 => WINDOWS_1251,
        _ => WINDOWS_1252,
    }
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn parse_hyperlink(value: &str) -> Option<String> {
    let value = value.trim();
    let rest = value
        .strip_prefix("HYPERLINK")
        .or_else(|| value.strip_prefix("hyperlink"))?
        .trim();
    if let Some(rest) = rest.strip_prefix("\\l") {
        return quoted(rest.trim()).map(|anchor| format!("#{anchor}"));
    }
    quoted(rest).or_else(|| rest.split_whitespace().next().map(ToOwned::to_owned))
}

fn quoted(value: &str) -> Option<String> {
    let start = value.find('"')? + 1;
    let end = value[start..].find('"')? + start;
    Some(value[start..end].to_owned())
}
