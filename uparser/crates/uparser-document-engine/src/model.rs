use crate::{DocumentFormat, ParseWarning};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub type AnchorId = String;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanonicalDocument {
    pub schema_version: String,
    pub metadata: DocumentMetadata,
    pub units: Vec<DocumentUnit>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<Note>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assets: Vec<Asset>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<ParseWarning>,
}

impl CanonicalDocument {
    pub fn new(format: DocumentFormat) -> Self {
        Self {
            schema_version: "uparser.document.v1".to_owned(),
            metadata: DocumentMetadata {
                format,
                variant: None,
                title: None,
                author: None,
                subject: None,
                language: None,
                created_at: None,
                modified_at: None,
                properties: BTreeMap::new(),
            },
            units: Vec::new(),
            notes: Vec::new(),
            assets: Vec::new(),
            warnings: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.units.iter().all(|unit| unit.blocks.is_empty())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentMetadata {
    pub format: DocumentFormat,
    pub variant: Option<String>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub language: Option<String>,
    pub created_at: Option<String>,
    pub modified_at: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitKind {
    Flow,
    Page,
    Slide,
    Sheet,
    Chapter,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentUnit {
    pub kind: UnitKind,
    pub index: usize,
    pub label: Option<String>,
    #[serde(default)]
    pub blocks: Vec<Block>,
}

impl DocumentUnit {
    pub fn new(kind: UnitKind, index: usize, label: Option<String>) -> Self {
        Self {
            kind,
            index,
            label,
            blocks: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Heading {
        level: u8,
        content: Vec<Inline>,
    },
    Paragraph {
        content: Vec<Inline>,
    },
    List {
        list: List,
    },
    Table {
        table: Table,
    },
    BlockQuote {
        blocks: Vec<Block>,
    },
    CodeBlock {
        language: Option<String>,
        text: String,
    },
    Figure {
        asset_id: Option<AssetId>,
        alt: Option<String>,
        caption: Vec<Inline>,
    },
    Rule,
}

impl Block {
    pub fn paragraph(text: impl Into<String>) -> Self {
        Self::Paragraph {
            content: vec![Inline::Text {
                text: text.into(),
                style: Style::default(),
            }],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Inline {
    Text {
        text: String,
        style: Style,
    },
    Link {
        target: LinkTarget,
        content: Vec<Inline>,
    },
    Image {
        source: ImageSource,
        alt: Option<String>,
    },
    Anchor {
        id: AnchorId,
    },
    NoteRef {
        id: String,
    },
    LineBreak,
    Formula {
        source: FormulaSource,
        display: Option<String>,
    },
}

impl Inline {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text {
            text: text.into(),
            style: Style::default(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Style {
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
    #[serde(default)]
    pub underline: bool,
    #[serde(default)]
    pub strike: bool,
    #[serde(default)]
    pub code: bool,
    pub superscript: Option<bool>,
    pub language: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum LinkTarget {
    External(String),
    Anchor(AnchorId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum FormulaSource {
    Spreadsheet(String),
    MathMl(String),
    Latex(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListMarker {
    Bullet,
    Decimal,
    LowerAlpha,
    UpperAlpha,
    LowerRoman,
    UpperRoman,
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct List {
    pub marker: ListMarker,
    pub start: Option<u64>,
    #[serde(default)]
    pub items: Vec<ListItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListItem {
    #[serde(default)]
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TableKind {
    Data,
    Layout,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Table {
    pub kind: TableKind,
    pub rows: usize,
    pub columns: usize,
    pub header_rows: usize,
    #[serde(default)]
    pub grid: Vec<Vec<CellSlot>>,
    pub caption: Option<Vec<Inline>>,
}

impl Table {
    pub fn has_spans(&self) -> bool {
        self.grid.iter().flatten().any(|slot| match slot {
            CellSlot::Origin(cell) => cell.row_span > 1 || cell.column_span > 1,
            CellSlot::Covered { .. } => true,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "slot", rename_all = "snake_case")]
pub enum CellSlot {
    Origin(Cell),
    Covered {
        origin_row: usize,
        origin_column: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    pub row_span: usize,
    pub column_span: usize,
    pub value_kind: CellValueKind,
    pub formula: Option<FormulaSource>,
    #[serde(default)]
    pub blocks: Vec<Block>,
}

impl Cell {
    pub fn text(text: impl Into<String>, value_kind: CellValueKind) -> Self {
        let text = text.into();
        Self {
            row_span: 1,
            column_span: 1,
            value_kind,
            formula: None,
            blocks: if text.is_empty() {
                Vec::new()
            } else {
                vec![Block::paragraph(text)]
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CellValueKind {
    Empty,
    Text,
    Number,
    Boolean,
    DateTime,
    Error,
}

pub type AssetId = String;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Asset {
    pub id: AssetId,
    pub media_type: String,
    pub filename: Option<String>,
    pub byte_length: usize,
    pub sha256: String,
    /// Where the caller wrote this asset, relative to the output document.
    /// Empty until something materialises it; the Markdown renderer points
    /// `![](…)` here when it is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Raw bytes, for a caller that wants to write the asset out.
    ///
    /// **Never serialized.** `Vec<u8>` round-trips through JSON as an array
    /// of numbers, which inflates a document's JSON by several times its own
    /// size for content the consumer cannot use as an image anyway; callers
    /// take the bytes from the in-memory model and reference `path` instead.
    #[serde(skip)]
    pub bytes: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ImageSource {
    Asset(AssetId),
    External(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteKind {
    Footnote,
    Endnote,
    Comment,
    SpeakerNote,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    pub kind: NoteKind,
    #[serde(default)]
    pub blocks: Vec<Block>,
}
