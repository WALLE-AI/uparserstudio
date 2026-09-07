//! Shared types used across the extraction and markdown pipelines.
//!
//! Centralises `TextItem`, `TextLine`, `PdfRect`, font-width / encoding
//! type aliases, and the `ItemType` enum so that every module can import
//! them from one place.

use std::collections::HashMap;

use crate::text_utils::should_join_items;

/// Result tuple returned by page-level text extraction: text items, rectangles, line segments,
/// and whether fonts with unresolvable gid-encoded glyphs were encountered.
pub(crate) type PageExtraction = (Vec<TextItem>, Vec<PdfRect>, Vec<PdfLine>);

// ── Font types (crate-internal) ──────────────────────────────────────

/// Font encoding map: maps byte codes to Unicode characters
pub(crate) type FontEncodingMap = HashMap<u8, char>;

/// All font encodings for a page
pub(crate) type PageFontEncodings = HashMap<String, FontEncodingMap>;

/// Font width information extracted from PDF font dictionaries
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct FontWidthInfo {
    /// Glyph widths: maps character code to width in font units
    pub(crate) widths: HashMap<u16, u16>,
    /// Default width for glyphs not in the widths table
    pub(crate) default_width: u16,
    /// Width of the space character (code 32) if known
    pub(crate) space_width: u16,
    /// Whether this is a CID font (2-byte character codes)
    pub(crate) is_cid: bool,
    /// Scale factor to convert font units to text space units.
    /// For Type1/TrueType: 0.001 (widths in 1000ths of em)
    /// For Type3: FontMatrix[0] (e.g., 0.00048828125 for 2048-unit grid)
    pub(crate) units_scale: f32,
    /// Writing mode: 0 = horizontal (default), 1 = vertical
    pub(crate) wmode: u8,
}

/// All font width info for a page, keyed by font resource name
pub(crate) type PageFontWidths = HashMap<String, FontWidthInfo>;

// ── Public types ─────────────────────────────────────────────────────

/// Type of extracted item
#[derive(Debug, Clone, Default)]
pub enum ItemType {
    /// Regular text content
    #[default]
    Text,
    /// Image placeholder
    Image,
    /// Hyperlink (with URL)
    Link(String),
    /// Form field (name: value)
    FormField,
}

/// Layout complexity analysis result.
///
/// Callers can use this to decide whether the extracted markdown is reliable
/// or whether the PDF should be routed to an OCR pipeline instead.
#[derive(Debug, Clone, Default)]
pub struct LayoutComplexity {
    /// True if any page has tables or multi-column text.
    pub is_complex: bool,
    /// 1-indexed pages where table borders were detected (rect count > 6).
    pub pages_with_tables: Vec<u32>,
    /// 1-indexed pages where 2+ text columns were detected.
    pub pages_with_columns: Vec<u32>,
}

/// A line segment from PDF path operators (`m`/`l`/`S`).
#[derive(Debug, Clone)]
pub struct PdfLine {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
    pub page: u32,
}

/// Bounding geometry of a path that was actually painted by a PDF content
/// stream. Control points are included in the bounds, which is conservative
/// for cubic Bezier curves and avoids losing curved plots and diagrams.
#[derive(Debug, Clone)]
pub(crate) struct PdfPaintedPath {
    pub bbox: [f32; 4],
    pub page: u32,
    pub segment_count: u32,
    pub has_curve: bool,
    pub has_diagonal: bool,
}

/// A rectangle from a PDF `re` operator (cell boundary, border, etc.)
#[derive(Debug, Clone)]
pub struct PdfRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub page: u32,
}

/// A text item with position information
#[derive(Debug, Clone)]
pub struct TextItem {
    /// The text content
    pub text: String,
    /// X position on page
    pub x: f32,
    /// Y position on page (PDF coordinates, origin at bottom-left)
    pub y: f32,
    /// Width of text
    pub width: f32,
    /// Height (approximated from font size)
    pub height: f32,
    /// Font name
    pub font: String,
    /// Font size
    pub font_size: f32,
    /// Page number (1-indexed)
    pub page: u32,
    /// Whether the font is bold
    pub is_bold: bool,
    /// Whether the font is italic
    pub is_italic: bool,
    /// Whether the text is underlined (drawn rule/thin rect under the
    /// baseline — PDFs have no underline font flag, so this is detected
    /// geometrically after extraction; see `extractor::underline`).
    pub is_underline: bool,
    /// Whether the text is struck out (drawn rule/thin rect crossing the
    /// glyphs at mid x-height). Same geometric detection as underline,
    /// different vertical window; see `extractor::underline`.
    pub is_strikeout: bool,
    /// Type of item (text, image, link)
    pub item_type: ItemType,
    /// Marked Content ID from the content stream's BDC/BMC operator.
    /// Used to link this item to the PDF structure tree for tagged PDFs.
    pub mcid: Option<i64>,
}

/// A line of text (grouped text items)
#[derive(Debug, Clone)]
pub struct TextLine {
    pub items: Vec<TextItem>,
    pub y: f32,
    pub page: u32,
    /// Adaptive join threshold from page-level letter-spacing detection.
    /// Default 0.10 for normal PDFs; higher for Canva-style PDFs.
    #[doc(hidden)]
    pub adaptive_threshold: f32,
}

impl TextLine {
    pub fn text(&self) -> String {
        self.text_with_formatting(false, false, false)
    }

    /// Get text with optional bold/italic/underline markdown formatting
    pub fn text_with_formatting(
        &self,
        format_bold: bool,
        format_italic: bool,
        format_underline: bool,
    ) -> String {
        if !format_bold && !format_italic && !format_underline {
            return self.text_plain();
        }

        let single_char_threshold = self.adaptive_threshold;

        let mut result = String::new();
        let mut current_bold = false;
        let mut current_italic = false;
        let mut current_underline = false;

        for (i, item) in self.items.iter().enumerate() {
            let text = item.text.as_str();
            let text_trimmed = text.trim();

            // Skip empty items
            if text_trimmed.is_empty() {
                continue;
            }

            // Determine spacing
            let needs_space = if i == 0 || result.is_empty() {
                false
            } else {
                let prev_item = &self.items[i - 1];
                self.needs_space_between(prev_item, item, &result, single_char_threshold)
            };

            // Preserve leading whitespace from the item text.
            // Items like " means any person" have a leading space that indicates
            // a word boundary. needs_space_between returns false for these (because
            // space_already_exists), but we still need to emit the space since
            // we push text_trimmed below (which strips it).
            let has_leading_space = text.starts_with(' ');

            // Check for style changes. Underline is exclusive: `<u>` content
            // stays free of `**`/`*` markers — consumers (and the eval
            // harnesses this feeds) match the tag content literally, and
            // mixed `<u>**x**</u>` nesting breaks that.
            let item_underline = format_underline && item.is_underline;
            let item_bold = format_bold && item.is_bold && !item_underline;
            let item_italic = format_italic && item.is_italic && !item_underline;

            // Close previous styles if they change
            if current_italic && !item_italic {
                result.push('*');
                current_italic = false;
            }
            if current_bold && !item_bold {
                result.push_str("**");
                current_bold = false;
            }
            if current_underline && !item_underline {
                result.push_str("</u>");
                current_underline = false;
            }

            // Add space: either from spacing logic or preserved from item text
            if needs_space || (has_leading_space && !result.is_empty() && !result.ends_with(' ')) {
                result.push(' ');
            }

            // Open new styles
            if item_underline && !current_underline {
                result.push_str("<u>");
                current_underline = true;
            }
            if item_bold && !current_bold {
                result.push_str("**");
                current_bold = true;
            }
            if item_italic && !current_italic {
                result.push('*');
                current_italic = true;
            }

            result.push_str(text_trimmed);
        }

        // Close any remaining open styles
        if current_italic {
            result.push('*');
        }
        if current_bold {
            result.push_str("**");
        }
        if current_underline {
            result.push_str("</u>");
        }

        result
    }

    /// Get plain text without formatting
    fn text_plain(&self) -> String {
        self.text_plain_pieces().concat()
    }

    /// The same plain text, split into the contribution of each item that
    /// carries any non-whitespace text.
    ///
    /// `pieces.concat() == self.text()` always. Each piece includes the
    /// separator the join chose before it, and the contribution of any
    /// whitespace-only item is folded into the piece that follows it (or the
    /// one before it, at the end of the line) — so the vector lines up with
    /// the items a consumer keeps, which is the non-blank ones. Nothing is
    /// dropped; a blank item's own spacing still reaches the text.
    ///
    /// Exists for consumers that need to attribute the joined text back to
    /// the items it came from — a span carrying that item's font style, say
    /// — without reimplementing the spacing rules, which are the accumulated
    /// answer to letter-spaced glyph runs, CID fonts, sub/superscripts and
    /// hyphenation and are not reproducible from item geometry alone.
    pub fn text_plain_pieces(&self) -> Vec<String> {
        let single_char_threshold = self.adaptive_threshold;

        let mut pieces: Vec<String> = Vec::with_capacity(self.items.len());
        let mut pending = String::new();
        let mut result = String::new();
        for (i, item) in self.items.iter().enumerate() {
            let text = item.text.as_str();
            // Everything this item adds to the running text: the separator
            // the join rules chose, then the item's own text.
            let mut added = String::new();
            if i > 0 {
                let prev_item = &self.items[i - 1];
                if self.needs_space_between(prev_item, item, &result, single_char_threshold) {
                    added.push(' ');
                }
            }
            added.push_str(text);
            result.push_str(&added);

            let mut piece = std::mem::take(&mut pending);
            piece.push_str(&added);
            if text.trim().is_empty() {
                pending = piece;
            } else {
                pieces.push(piece);
            }
        }
        if !pending.is_empty() {
            match pieces.last_mut() {
                Some(last) => last.push_str(&pending),
                None => pieces.push(pending),
            }
        }
        pieces
    }

    /// Determine if a space is needed between two items
    fn needs_space_between(
        &self,
        prev_item: &TextItem,
        item: &TextItem,
        result: &str,
        single_char_threshold: f32,
    ) -> bool {
        let text = item.text.as_str();

        // Don't add space before/after hyphens for hyphenated words
        let prev_ends_with_hyphen = result.ends_with('-');
        let curr_is_hyphen = text.trim() == "-";
        let curr_starts_with_hyphen = text.starts_with('-');

        // Detect subscript/superscript: smaller font size and/or Y offset
        let font_ratio = item.font_size / prev_item.font_size;
        let reverse_font_ratio = prev_item.font_size / item.font_size;
        let y_diff = (item.y - prev_item.y).abs();

        let is_sub_super = font_ratio < 0.85 && y_diff > 1.0;
        let was_sub_super = reverse_font_ratio < 0.85 && y_diff > 1.0;

        // Use position-based spacing detection
        let should_join = should_join_items(prev_item, item, single_char_threshold);

        // Check if space already exists
        let prev_ends_with_space = result.ends_with(' ');
        let curr_starts_with_space = text.starts_with(' ');
        let space_already_exists = prev_ends_with_space || curr_starts_with_space;

        // Add space unless one of these conditions applies
        !(prev_ends_with_hyphen
            || curr_is_hyphen
            || curr_starts_with_hyphen
            || is_sub_super
            || was_sub_super
            || should_join
            || space_already_exists)
    }
}

#[cfg(test)]
mod line_piece_tests {
    use super::*;

    fn item(text: &str, x: f32, width: f32) -> TextItem {
        TextItem {
            text: text.to_owned(),
            x,
            y: 100.0,
            width,
            height: 10.0,
            font: "F1".to_owned(),
            font_size: 10.0,
            page: 1,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            item_type: ItemType::Text,
            mcid: None,
        }
    }

    fn line(items: Vec<TextItem>) -> TextLine {
        TextLine {
            items,
            y: 100.0,
            page: 1,
            adaptive_threshold: 0.10,
        }
    }

    /// The contract consumers rely on: the pieces are the text, split up.
    #[test]
    fn pieces_always_concatenate_back_to_the_line_text() {
        let line = line(vec![
            item("Hello", 10.0, 25.0),
            item("world", 40.0, 25.0),
            item(".", 65.0, 3.0),
        ]);
        assert_eq!(line.text_plain_pieces().concat(), line.text());
    }

    /// A blank item has no piece of its own — a consumer drops those items —
    /// but whatever it contributed to the text still has to be there.
    #[test]
    fn a_blank_item_folds_into_its_neighbour_without_losing_text() {
        let line = line(vec![
            item("Hello", 10.0, 25.0),
            item(" ", 35.0, 4.0),
            item("world", 40.0, 25.0),
        ]);
        let pieces = line.text_plain_pieces();
        assert_eq!(pieces.len(), 2, "one piece per non-blank item");
        assert_eq!(pieces.concat(), line.text());
    }

    #[test]
    fn a_line_of_only_blank_items_yields_at_most_one_piece() {
        let line = line(vec![item("  ", 10.0, 8.0), item(" ", 20.0, 4.0)]);
        assert_eq!(line.text_plain_pieces().concat(), line.text());
    }
}
