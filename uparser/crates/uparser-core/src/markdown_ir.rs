//! Markdown → `Block` IR, for services that are authoritative for Markdown.
//!
//! # Why this exists
//!
//! Most protocols return structure (boxes, categories, cells) and this
//! project renders it. A few return the *finished document* as Markdown —
//! PaddleX PP-StructureV3's `/layout-parsing` is the one in use today, and
//! Pipeline V2's `markdown` response fields are shaped the same way.
//!
//! Putting that string straight into `Block.text` looks like the cheap
//! option and is not: the IR's contract is that `text` is plain text and
//! structure lives in fields, so every downstream pass treats the Markdown
//! as prose. `content_normalize` folds `\n\n` into `\n` (blank lines gone,
//! so headings and tables stop parsing) and the renderer escapes the syntax
//! (`# Heading` → `\# Heading`, `**bold**` → `\*\*bold\*\*`). Both are
//! correct behaviour for plain text; the input was mislabelled.
//!
//! Recovering the structure here is the same move `ascend.rs` already makes
//! for a model that writes `- ` into a `text` block: the IR ends up
//! truthful *and* the document renders correctly. It also means such a
//! protocol carries real `title`/`table` blocks, so it can be scored on the
//! same benchmark as every other one instead of reporting an empty IR.
//!
//! # Scope
//!
//! CommonMark plus tables, strikethrough and `$`-delimited math — the
//! subset these services actually emit. Constructs the `Block` IR has no
//! category for (code blocks, block quotes, thematic breaks) degrade to
//! text rather than being dropped.

use crate::types::{Block, BlockSource, CoordFrame, Geometry, MergeHint, Span, SpanStyle};
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

/// Parse `markdown` into IR blocks laid out on a page of `width` × `height`.
///
/// The envelope carries no per-block geometry, so `bbox_px` is `None`
/// throughout: claiming the page box for every block would be a fabricated
/// coordinate, and the passes that read `bbox_px` (paragraph merging,
/// geometric reading order) would act on it. `geom` has no `None` to return,
/// so it names the page — the one true statement available about where a
/// block sits.
pub fn blocks_from_markdown(markdown: &str, width: u32, height: u32) -> Vec<Block> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_MATH);
    Builder::new(width, height).run(Parser::new_ext(markdown, options))
}

struct Builder {
    width: u32,
    height: u32,
    blocks: Vec<Block>,
    /// Inline runs of the block currently being built.
    spans: Vec<Span>,
    style: SpanStyle,
    /// `Some(level)` while inside a heading.
    heading: Option<u8>,
    /// Innermost-last: each entry is `(ordered, next number)`. Nested lists
    /// need the stack because an item's depth is what the IR records.
    lists: Vec<(bool, u64)>,
    table: Option<TableBuilder>,
    /// A fenced code block's language, while inside one.
    code_block: Option<Option<String>>,
    /// Set when the current paragraph turned out to be exactly one image.
    image: Option<(String, Option<String>)>,
}

#[derive(Default)]
struct TableBuilder {
    rows: Vec<Vec<String>>,
    header_rows: usize,
    in_head: bool,
    cell: Option<String>,
}

impl Builder {
    fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            blocks: Vec::new(),
            spans: Vec::new(),
            style: SpanStyle::default(),
            heading: None,
            lists: Vec::new(),
            table: None,
            code_block: None,
            image: None,
        }
    }

    fn run(mut self, parser: Parser<'_>) -> Vec<Block> {
        for event in parser {
            self.event(event);
        }
        // A truncated document can end mid-block; emit what was collected
        // rather than dropping it.
        self.flush_text("text", None);
        for (index, block) in self.blocks.iter_mut().enumerate() {
            block.reading_order = Some(index as u32);
        }
        self.blocks
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.push_text(&text),
            // Inline code has no IR representation of its own; its content is
            // still content.
            Event::Code(text) => self.push_text(&text),
            Event::InlineMath(latex) => self.push_text(&format!("${latex}$")),
            Event::DisplayMath(latex) => self.emit_formula(&latex),
            Event::SoftBreak | Event::HardBreak => self.push_text("\n"),
            // A rule carries no content and the IR has no category for it.
            Event::Rule => {}
            // Raw HTML in a Markdown envelope is the service passing through
            // something it could not express; keep the text, drop the markup.
            Event::Html(html) | Event::InlineHtml(html) => {
                let stripped = strip_tags(&html);
                if !stripped.trim().is_empty() {
                    self.push_text(&stripped);
                }
            }
            Event::FootnoteReference(label) => self.push_text(&format!("[^{label}]")),
            Event::TaskListMarker(_) => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Heading { level, .. } => self.heading = Some(level as u8),
            Tag::Strong => self.style.bold = true,
            Tag::Emphasis => self.style.italic = true,
            Tag::Strikethrough => self.style.strike = true,
            Tag::List(start) => {
                // A nested list interrupts its parent item, whose own text
                // is already accumulated: "- outer\n  - inner" means an item
                // reading "outer" followed by a sublist, not one item
                // reading "outerinner". Close the parent here, at its real
                // depth, before the nesting changes what depth means.
                if !self.lists.is_empty() && !self.spans.is_empty() {
                    let level = self.lists.len().saturating_sub(1) as u8;
                    let (ordered, number) = self.next_item_number();
                    self.flush_text(
                        "list",
                        Some(MergeHint::ListItem {
                            ordered,
                            number,
                            level,
                        }),
                    );
                }
                self.lists.push((start.is_some(), start.unwrap_or(1)))
            }
            Tag::Table(_) => self.table = Some(TableBuilder::default()),
            Tag::TableHead => {
                if let Some(table) = &mut self.table {
                    table.in_head = true;
                    table.rows.push(Vec::new());
                }
            }
            Tag::TableRow => {
                if let Some(table) = &mut self.table {
                    table.rows.push(Vec::new());
                }
            }
            Tag::TableCell => {
                if let Some(table) = &mut self.table {
                    table.cell = Some(String::new());
                }
            }
            Tag::CodeBlock(kind) => {
                self.code_block = Some(match kind {
                    CodeBlockKind::Fenced(language) if !language.is_empty() => {
                        Some(language.into_string())
                    }
                    _ => None,
                })
            }
            Tag::Image {
                dest_url, title, ..
            } => {
                self.image = Some((
                    dest_url.into_string(),
                    Some(title.into_string()).filter(|title| !title.is_empty()),
                ));
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Heading(_) => {
                let level = self.heading.take().unwrap_or(1).clamp(1, 6);
                self.flush_text("title", Some(MergeHint::TitleLevel(level)));
            }
            TagEnd::Strong => self.style.bold = false,
            TagEnd::Emphasis => self.style.italic = false,
            TagEnd::Strikethrough => self.style.strike = false,
            TagEnd::Paragraph => self.flush_paragraph(),
            TagEnd::List(_) => {
                self.lists.pop();
            }
            TagEnd::Item => {
                let level = self.lists.len().saturating_sub(1) as u8;
                let (ordered, number) = self.next_item_number();
                self.flush_text(
                    "list",
                    Some(MergeHint::ListItem {
                        ordered,
                        number,
                        level,
                    }),
                );
            }
            TagEnd::TableCell => {
                if let Some(table) = &mut self.table
                    && let Some(cell) = table.cell.take()
                {
                    let cell = if cell.is_empty() {
                        std::mem::take(&mut self.spans)
                            .iter()
                            .map(|span| span.text.as_str())
                            .collect()
                    } else {
                        cell
                    };
                    if let Some(row) = table.rows.last_mut() {
                        row.push(cell);
                    }
                }
            }
            TagEnd::TableHead => {
                if let Some(table) = &mut self.table {
                    table.in_head = false;
                    table.header_rows = table.rows.len();
                }
            }
            TagEnd::Table => self.flush_table(),
            TagEnd::CodeBlock => {
                self.code_block = None;
                self.flush_text("text", None);
            }
            _ => {}
        }
    }

    /// Consume the innermost list's next ordinal. `None` for a bulleted
    /// list, where a number would be invented rather than read.
    fn next_item_number(&mut self) -> (bool, Option<u64>) {
        match self.lists.last_mut() {
            Some((ordered, next)) => {
                let number = *next;
                *next += 1;
                (*ordered, ordered.then_some(number))
            }
            None => (false, None),
        }
    }

    fn push_text(&mut self, text: &str) {
        if let Some(table) = &mut self.table
            && let Some(cell) = &mut table.cell
        {
            cell.push_str(text);
            return;
        }
        self.spans.push(Span {
            text: text.to_owned(),
            bbox_px: None,
            font_size: None,
            is_inline_formula: false,
            style: self.style,
        });
    }

    /// A paragraph that is exactly one image is a figure, not prose.
    fn flush_paragraph(&mut self) {
        if let Some((url, title)) = self.image.take() {
            let alt: String = self.spans.iter().map(|span| span.text.as_str()).collect();
            self.spans.clear();
            let caption = title.or_else(|| (!alt.trim().is_empty()).then(|| alt.clone()));
            let mut block = self.skeleton();
            block.category_raw = "image".into();
            block.category = Some("image".into());
            block.asset_path = Some(url);
            block.asset_caption = caption.map(|text| crate::types::AssetCaption {
                text,
                bbox_px: None,
                confidence: 1.0,
            });
            self.blocks.push(block);
            return;
        }
        self.flush_text("text", None);
    }

    fn emit_formula(&mut self, latex: &str) {
        self.flush_text("text", None);
        let mut block = self.skeleton();
        block.category_raw = "equation".into();
        block.category = Some("equation".into());
        block.latex = Some(latex.trim().to_owned());
        self.blocks.push(block);
    }

    fn flush_table(&mut self) {
        let Some(table) = self.table.take() else {
            return;
        };
        if table.rows.iter().all(Vec::is_empty) {
            return;
        }
        let mut html = String::from("<table>");
        for (index, row) in table.rows.iter().enumerate() {
            let header = index < table.header_rows;
            if header && index == 0 {
                html.push_str("<thead>");
            }
            if !header && index == table.header_rows && table.header_rows > 0 {
                html.push_str("</thead><tbody>");
            }
            html.push_str("<tr>");
            for cell in row {
                let tag = if header { "th" } else { "td" };
                html.push('<');
                html.push_str(tag);
                html.push('>');
                html.push_str(&crate::otsl::escape_html(cell.trim()));
                html.push_str("</");
                html.push_str(tag);
                html.push('>');
            }
            html.push_str("</tr>");
        }
        if table.header_rows > 0 && table.rows.len() > table.header_rows {
            html.push_str("</tbody>");
        } else if table.header_rows > 0 {
            html.push_str("</thead>");
        }
        html.push_str("</table>");

        let mut block = self.skeleton();
        block.category_raw = "table".into();
        block.category = Some("table".into());
        block.html = Some(html);
        self.blocks.push(block);
    }

    /// Emit whatever inline runs have accumulated as one block.
    fn flush_text(&mut self, category: &str, merge_hint: Option<MergeHint>) {
        let spans = std::mem::take(&mut self.spans);
        let text: String = spans.iter().map(|span| span.text.as_str()).collect();
        if text.trim().is_empty() {
            return;
        }
        let mut block = self.skeleton();
        block.category_raw = category.into();
        block.category = Some(category.into());
        block.text = Some(text.trim().to_owned());
        block.merge_hint = merge_hint;
        // The spans have to keep reproducing the text or a consumer will not
        // trust them (`ascend::inline_content`); trimming the text without
        // trimming them breaks that.
        block.spans = trim_spans(spans);
        self.blocks.push(block);
    }

    fn skeleton(&self) -> Block {
        Block {
            geom: Geometry::Rect([0.0, 0.0, self.width as f32, self.height as f32]),
            geom_frame: CoordFrame::Page,
            bbox_px: None,
            category_raw: String::new(),
            category: None,
            reading_order: None,
            text: None,
            html: None,
            latex: None,
            spans: Vec::new(),
            merge_hint: None,
            confidence: None,
            source: BlockSource::StructuredService,
            error: None,
            asset_bytes: None,
            asset_path: None,
            asset_caption: None,
        }
    }
}

/// Trim the span sequence the same way the block's text was trimmed, so the
/// two still concatenate to the same string.
fn trim_spans(mut spans: Vec<Span>) -> Vec<Span> {
    while let Some(first) = spans.first_mut() {
        first.text = first.text.trim_start().to_owned();
        if first.text.is_empty() {
            spans.remove(0);
        } else {
            break;
        }
    }
    while let Some(last) = spans.last_mut() {
        last.text = last.text.trim_end().to_owned();
        if last.text.is_empty() {
            spans.pop();
        } else {
            break;
        }
    }
    spans
}

fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut depth = 0usize;
    for character in html.chars() {
        match character {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(character),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(markdown: &str) -> Vec<Block> {
        blocks_from_markdown(markdown, 800, 1000)
    }

    fn shape(blocks: &[Block]) -> Vec<(&str, Option<&str>)> {
        blocks
            .iter()
            .map(|block| {
                (
                    block.category.as_deref().unwrap_or(""),
                    block.text.as_deref(),
                )
            })
            .collect()
    }

    #[test]
    fn heading_becomes_a_title_block_carrying_its_level() {
        let blocks = parse("# One\n\n### Three\n");
        assert_eq!(
            shape(&blocks),
            [("title", Some("One")), ("title", Some("Three"))]
        );
        assert_eq!(blocks[0].merge_hint, Some(MergeHint::TitleLevel(1)));
        assert_eq!(blocks[1].merge_hint, Some(MergeHint::TitleLevel(3)));
    }

    /// The defect this module exists for: the markup must reach the IR as
    /// structure, never as characters inside `text`.
    #[test]
    fn no_markdown_syntax_survives_in_block_text() {
        let blocks = parse(
            "# Heading\n\nBody with **bold**\n\n- one\n- two\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n",
        );
        for block in &blocks {
            let Some(text) = block.text.as_deref() else {
                continue;
            };
            for marker in ['#', '*', '|'] {
                assert!(
                    !text.contains(marker),
                    "{marker:?} left in text: {text:?} ({:?})",
                    block.category
                );
            }
        }
    }

    #[test]
    fn emphasis_becomes_span_style_not_asterisks() {
        let blocks = parse("Body with **bold** and *italic* here\n");
        assert_eq!(
            blocks[0].text.as_deref(),
            Some("Body with bold and italic here")
        );
        let bold: Vec<&str> = blocks[0]
            .spans
            .iter()
            .filter(|span| span.style.bold)
            .map(|span| span.text.as_str())
            .collect();
        assert_eq!(bold, ["bold"]);
        let italic: Vec<&str> = blocks[0]
            .spans
            .iter()
            .filter(|span| span.style.italic)
            .map(|span| span.text.as_str())
            .collect();
        assert_eq!(italic, ["italic"]);
    }

    /// `ascend::inline_content` only trusts spans that rebuild the text
    /// exactly; a block whose spans disagree silently loses all its styling.
    #[test]
    fn spans_always_concatenate_back_to_the_block_text() {
        for markdown in [
            "  padded **bold** text  \n",
            "# *Heading*\n",
            "- item with **bold**\n",
        ] {
            for block in parse(markdown) {
                let Some(text) = block.text.as_deref() else {
                    continue;
                };
                let rebuilt: String = block.spans.iter().map(|s| s.text.as_str()).collect();
                assert_eq!(rebuilt, text, "for {markdown:?}");
            }
        }
    }

    #[test]
    fn list_items_become_list_blocks_without_their_markers() {
        let blocks = parse("- alpha\n- beta\n");
        assert_eq!(
            shape(&blocks),
            [("list", Some("alpha")), ("list", Some("beta"))]
        );
        assert!(matches!(
            blocks[0].merge_hint,
            Some(MergeHint::ListItem {
                ordered: false,
                number: None,
                level: 0
            })
        ));
    }

    #[test]
    fn ordered_list_numbers_come_from_the_source_not_from_a_counter() {
        let blocks = parse("3. three\n4. four\n");
        assert!(matches!(
            blocks[0].merge_hint,
            Some(MergeHint::ListItem {
                ordered: true,
                number: Some(3),
                ..
            })
        ));
        assert!(matches!(
            blocks[1].merge_hint,
            Some(MergeHint::ListItem {
                number: Some(4),
                ..
            })
        ));
    }

    #[test]
    fn nested_list_items_record_their_depth() {
        let blocks = parse("- outer\n  - inner\n");
        let levels: Vec<u8> = blocks
            .iter()
            .filter_map(|block| match block.merge_hint {
                Some(MergeHint::ListItem { level, .. }) => Some(level),
                _ => None,
            })
            .collect();
        assert_eq!(
            levels,
            [0, 1],
            "the outer item's own text closes before the sublist opens, so \
             the two arrive in document order at their real depths"
        );
    }

    #[test]
    fn table_becomes_html_with_a_header_row() {
        let blocks = parse("| a | b |\n| --- | --- |\n| 1 | 2 |\n");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].category.as_deref(), Some("table"));
        let html = blocks[0].html.as_deref().unwrap();
        assert!(
            html.contains("<thead><tr><th>a</th><th>b</th></tr>"),
            "{html}"
        );
        assert!(
            html.contains("<tbody><tr><td>1</td><td>2</td></tr>"),
            "{html}"
        );
        assert!(html.ends_with("</tbody></table>"), "{html}");
    }

    /// Cell text is escaped on the way into the HTML, and raw markup the
    /// service passed through does not become live markup in it — otherwise
    /// a cell reading `<b> &` would produce HTML that no longer parses back
    /// into the same table.
    #[test]
    fn table_cell_text_is_escaped_and_raw_markup_does_not_leak() {
        let blocks = parse("| a |\n| --- |\n| <b> & |\n");
        let html = blocks[0].html.as_deref().unwrap();
        assert!(html.contains("<td>&amp;</td>"), "{html}");
        assert!(!html.contains("<b>"), "{html}");
    }

    #[test]
    fn display_math_becomes_a_latex_block() {
        let blocks = parse("$$\n\\frac{a}{b}\n$$\n");
        assert_eq!(blocks[0].category.as_deref(), Some("equation"));
        assert_eq!(blocks[0].latex.as_deref(), Some("\\frac{a}{b}"));
        assert_eq!(blocks[0].text, None);
    }

    #[test]
    fn a_paragraph_that_is_one_image_becomes_an_image_block() {
        let blocks = parse("![Figure 1](imgs/plot.png)\n");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].category.as_deref(), Some("image"));
        assert_eq!(blocks[0].asset_path.as_deref(), Some("imgs/plot.png"));
        assert_eq!(
            blocks[0].asset_caption.as_ref().map(|c| c.text.as_str()),
            Some("Figure 1")
        );
        assert_eq!(blocks[0].text, None);
    }

    /// No geometry is invented: the envelope has none, and a fabricated
    /// page-sized box would be consumed by paragraph merging and by any
    /// geometric reading-order pass as if it were measured.
    #[test]
    fn blocks_carry_no_fabricated_bounding_box() {
        for block in parse("# One\n\nBody\n") {
            assert_eq!(block.bbox_px, None);
        }
    }

    #[test]
    fn reading_order_follows_document_order() {
        let blocks = parse("# One\n\nBody\n\n- item\n");
        let order: Vec<Option<u32>> = blocks.iter().map(|block| block.reading_order).collect();
        assert_eq!(order, [Some(0), Some(1), Some(2)]);
    }

    /// The IR has no category for these; losing the words would be worse
    /// than losing the block kind.
    #[test]
    fn code_and_quotes_degrade_to_text_instead_of_disappearing() {
        let blocks = parse("```rust\nlet x = 1;\n```\n\n> quoted line\n");
        let texts: Vec<&str> = blocks.iter().filter_map(|b| b.text.as_deref()).collect();
        assert!(texts.iter().any(|t| t.contains("let x = 1;")), "{texts:?}");
        assert!(texts.iter().any(|t| t.contains("quoted line")), "{texts:?}");
    }

    #[test]
    fn empty_or_whitespace_markdown_produces_no_blocks() {
        assert!(parse("").is_empty());
        assert!(parse("   \n\n  \n").is_empty());
    }

    #[test]
    fn arbitrary_input_never_panics() {
        for markdown in [
            "| unclosed",
            "# ",
            "***",
            "$$",
            "![](",
            "- \n- \n",
            "<table><tr><td>raw",
            "\u{0}\u{1}\u{feff}",
        ] {
            let _ = parse(markdown);
        }
    }
}
