//! Render a `ParseResult` to an output format. P0 covers markdown/json/
//! content-list only, without table HTML embedding or other advanced
//! formatting (those land alongside the adapters that produce the
//! richer signals they depend on).

use crate::types::ParseResult;

pub fn to_json(result: &ParseResult) -> String {
    serde_json::to_string_pretty(result).expect("ParseResult is always serializable")
}

pub fn to_markdown(result: &ParseResult) -> String {
    if matches!(
        result.protocol.as_str(),
        "mineru-vlm-official" | "mineru-vlm-surpass"
    ) {
        return to_mineru_official_markdown(result);
    }
    let mut out = String::new();
    for page in &result.pages {
        for block in &page.blocks {
            if is_discarded_markdown_category(block.category.as_deref()) {
                continue;
            }
            if let Some(html) = &block.html {
                out.push_str(html);
                out.push_str("\n\n");
            } else if let Some(latex) = &block.latex {
                out.push_str("$$\n");
                out.push_str(latex);
                out.push_str("\n$$\n\n");
            } else if let Some(text) = &block.text {
                // Emit semantic Markdown markup from the block's normalized
                // category so heading/list structure survives into Markdown
                // (previously every text block rendered as a bare paragraph,
                // which zeroed the heading-hierarchy metric for the VLM
                // protocols whose adapters DO classify titles/lists — see
                // the opendataloader-bench mineru-vlm finding). `native`'s
                // markdown path bypasses this renderer entirely, so it is
                // unaffected.
                match block.category.as_deref() {
                    Some("title") => {
                        out.push_str("# ");
                        out.push_str(text);
                    }
                    Some("list") => {
                        out.push_str("- ");
                        out.push_str(text);
                    }
                    _ => out.push_str(text),
                }
                out.push_str("\n\n");
            } else if let Some(asset_path) = &block.asset_path {
                // See `image_link_gap_report.md`: image-category blocks
                // previously fell through every branch above with
                // text/html/latex all `None`, producing no Markdown
                // output at all. `asset_path` is only ever populated
                // by `assets::write_page_assets` after a real crop was
                // written to disk.
                out.push_str("![](");
                out.push_str(asset_path);
                out.push_str(")\n\n");
            }
        }
    }
    out.trim_end().to_string()
}

/// OmniDocBench's official MinerU conversion writes each truthy `content`
/// field verbatim in model order, separated by blank lines. In particular,
/// it does not add Markdown heading/list syntax or wrap an already-delimited
/// equation in another pair of math delimiters.
fn to_mineru_official_markdown(result: &ParseResult) -> String {
    result
        .pages
        .iter()
        .flat_map(|page| &page.blocks)
        .filter_map(|block| {
            block
                .html
                .as_deref()
                .or(block.latex.as_deref())
                .or(block.text.as_deref())
        })
        .filter(|content| !content.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn is_discarded_markdown_category(category: Option<&str>) -> bool {
    matches!(
        category,
        Some("header" | "footer" | "page_number" | "unknown")
    )
}

pub fn to_content_list(result: &ParseResult) -> String {
    let items: Vec<serde_json::Value> = result
        .pages
        .iter()
        .flat_map(|page| {
            page.blocks.iter().map(move |block| {
                serde_json::json!({
                    "page_num": page.page_num,
                    "category": block.category,
                    "text": block.text,
                    "html": block.html,
                    "latex": block.latex,
                    "asset_path": block.asset_path,
                })
            })
        })
        .collect();
    serde_json::to_string_pretty(&items).expect("content list is always serializable")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;

    fn sample_result() -> ParseResult {
        ParseResult {
            source_path: "doc.pdf".into(),
            source_sha256: "abc123".into(),
            protocol: "mock".into(),
            routed_by: RoutedBy::Explicit,
            document_profile: None,
            model_endpoint: None,
            model_name: None,
            pages: vec![Page {
                page_num: 1,
                width_px: 100,
                height_px: 100,
                blocks: vec![Block {
                    geom: Geometry::Rect([0.0, 0.0, 10.0, 10.0]),
                    geom_frame: CoordFrame::Page,
                    bbox_px: Some([0, 0, 10, 10]),
                    category_raw: "text".into(),
                    category: Some("text".into()),
                    reading_order: Some(0),
                    text: Some("Hello world".into()),
                    html: None,
                    latex: None,
                    spans: vec![],
                    merge_hint: None,
                    confidence: Some(1.0),
                    source: BlockSource::OneShotVlm,
                    error: None,
                    asset_bytes: None,
                    asset_path: None,
                }],
            }],
            page_errors: vec![],
            capability_notes: vec![],
            warnings: vec![],
            timing: Default::default(),
        }
    }

    #[test]
    fn markdown_snapshot() {
        insta::assert_snapshot!(to_markdown(&sample_result()));
    }

    #[test]
    fn official_mineru_markdown_emits_content_verbatim() {
        let mut result = sample_result();
        result.protocol = "mineru-vlm-official".into();
        result.pages[0].blocks[0].category = Some("title".into());
        result.pages[0].blocks[0].text = Some("Heading".into());

        let mut formula = result.pages[0].blocks[0].clone();
        formula.text = None;
        formula.category = Some("equation".into());
        formula.latex = Some("\\[\nx+y\n\\]".into());
        result.pages[0].blocks.push(formula);

        assert_eq!(to_markdown(&result), "Heading\n\n\\[\nx+y\n\\]");
    }

    #[test]
    fn markdown_renders_an_image_only_block_as_a_link() {
        let mut result = sample_result();
        result.pages[0].blocks[0].text = None;
        result.pages[0].blocks[0].asset_path = Some("doc_images/abc123.png".into());
        assert_eq!(to_markdown(&result), "![](doc_images/abc123.png)");
    }

    #[test]
    fn markdown_prefers_text_over_asset_path_when_both_are_present() {
        // Shouldn't happen in practice (an adapter either extracts text
        // or crops an image, not both, for the same block) but the
        // fallback order should still be deterministic if it ever does.
        let mut result = sample_result();
        result.pages[0].blocks[0].asset_path = Some("doc_images/abc123.png".into());
        assert_eq!(to_markdown(&result), "Hello world");
    }

    #[test]
    fn content_list_includes_asset_path() {
        let mut result = sample_result();
        result.pages[0].blocks[0].text = None;
        result.pages[0].blocks[0].asset_path = Some("doc_images/abc123.png".into());
        let list = to_content_list(&result);
        assert!(list.contains("doc_images/abc123.png"));
    }

    #[test]
    fn content_list_snapshot() {
        insta::assert_snapshot!(to_content_list(&sample_result()));
    }

    #[test]
    fn json_round_trips() {
        let json = to_json(&sample_result());
        let back: ParseResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back.pages.len(), 1);
    }

    /// Gate G2 proof: this module is unmodified by P2. Build two
    /// `ParseResult`s with identical rendered content (text/html/latex)
    /// but mineru-vlm-shaped vs dots.ocr-shaped metadata (differing
    /// `source`, `category_raw` casing, `reading_order` presence) and
    /// assert every renderer produces the same output regardless — none
    /// of these functions branch on protocol-specific metadata.
    #[test]
    fn renderers_are_identical_across_differently_shaped_protocols() {
        fn block(source: BlockSource, category_raw: &str, reading_order: Option<u32>) -> Block {
            Block {
                geom: Geometry::Rect([0.0, 0.0, 10.0, 10.0]),
                geom_frame: CoordFrame::Page,
                bbox_px: Some([0, 0, 10, 10]),
                category_raw: category_raw.into(),
                category: Some("text".into()),
                reading_order,
                text: Some("Hello world".into()),
                html: None,
                latex: None,
                spans: vec![],
                merge_hint: None,
                confidence: None,
                source,
                error: None,
                asset_bytes: None,
                asset_path: None,
            }
        }

        fn result_with(block: Block) -> ParseResult {
            let mut r = sample_result();
            r.pages[0].blocks = vec![block];
            r
        }

        let mineru_shaped = result_with(block(BlockSource::LayoutThenRecognize, "text", None));
        let dots_ocr_shaped = result_with(block(BlockSource::OneShotVlm, "Text", Some(0)));

        assert_eq!(to_markdown(&mineru_shaped), to_markdown(&dots_ocr_shaped));
        assert_eq!(
            to_content_list(&mineru_shaped),
            to_content_list(&dots_ocr_shaped)
        );
    }

    /// The shared renderer emits Markdown markup from the normalized
    /// `category` — proven here at the renderer level so it holds for EVERY
    /// VLM adapter (mineru-vlm/dots.ocr/monkeyocr-v2/pipeline all normalize
    /// title/section-header → "title" and list-item → "list" via
    /// `category_map`). This is the fix behind mineru-vlm's 0.708→0.928 jump
    /// on opendataloader-bench (see `BENCHMARK_REPORT.md`).
    #[test]
    fn category_drives_heading_and_list_markdown_markup() {
        fn typed(category: &str, text: &str) -> Block {
            Block {
                geom: Geometry::Rect([0.0, 0.0, 10.0, 10.0]),
                geom_frame: CoordFrame::Page,
                bbox_px: Some([0, 0, 10, 10]),
                category_raw: category.into(),
                category: Some(category.into()),
                reading_order: None,
                text: Some(text.into()),
                html: None,
                latex: None,
                spans: vec![],
                merge_hint: None,
                confidence: None,
                source: BlockSource::LayoutThenRecognize,
                error: None,
                asset_bytes: None,
                asset_path: None,
            }
        }
        let mut r = sample_result();
        r.pages[0].blocks = vec![
            typed("title", "The Heading"),
            typed("list", "an item"),
            typed("text", "a paragraph"),
        ];
        let md = to_markdown(&r);
        assert!(md.contains("# The Heading"), "title → '# ': {md}");
        assert!(md.contains("- an item"), "list → '- ': {md}");
        // Plain text is unprefixed.
        assert!(md.contains("a paragraph") && !md.contains("# a paragraph"));
    }

    #[test]
    fn markdown_discards_paratext_categories() {
        fn typed(category: &str, text: &str) -> Block {
            Block {
                geom: Geometry::Rect([0.0, 0.0, 10.0, 10.0]),
                geom_frame: CoordFrame::Page,
                bbox_px: Some([0, 0, 10, 10]),
                category_raw: category.into(),
                category: Some(category.into()),
                reading_order: None,
                text: Some(text.into()),
                html: None,
                latex: None,
                spans: vec![],
                merge_hint: None,
                confidence: None,
                source: BlockSource::LayoutThenRecognize,
                error: None,
                asset_bytes: None,
                asset_path: None,
            }
        }
        let mut r = sample_result();
        r.pages[0].blocks = vec![
            typed("header", "running head"),
            typed("page_number", "12"),
            typed("text", "body"),
            typed("footer", "footer"),
        ];
        assert_eq!(to_markdown(&r), "body");
    }
}
