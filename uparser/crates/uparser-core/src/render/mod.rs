//! Render a `ParseResult` to an output format. P0 covers markdown/json/
//! content-list only, without table HTML embedding or other advanced
//! formatting (those land alongside the adapters that produce the
//! richer signals they depend on).

use crate::types::{MergeHint, ParseResult};

pub fn to_json(result: &ParseResult) -> String {
    serde_json::to_string_pretty(result).expect("ParseResult is always serializable")
}

pub fn to_markdown(result: &ParseResult) -> String {
    let mut out = String::new();
    for page in &result.pages {
        for block in &page.blocks {
            if let Some(html) = &block.html {
                out.push_str(html);
                out.push_str("\n\n");
            } else if let Some(latex) = &block.latex {
                // Inline formulas (`category == "equation_inline"`, e.g.
                // pipeline's `inline_formula` regions) render as `$...$`
                // so they stay inline with surrounding text; every other
                // formula renders as a display-math `$$...$$` block. See
                // D3 in `PIPELINE_V2_TABLE_OCR_DEFECT_ANALYSIS.md` — the
                // prior unconditional `$$...$$` split every paragraph
                // containing an inline formula into two blocks.
                if block.category.as_deref() == Some("equation_inline") {
                    out.push('$');
                    out.push_str(latex);
                    out.push('$');
                } else {
                    out.push_str("$$\n");
                    out.push_str(latex);
                    out.push_str("\n$$");
                }
                out.push_str("\n\n");
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
                        // `merge_hint::TitleLevel(n)` (currently only set
                        // by the `pipeline` adapter's doc_title/
                        // paragraph_title distinction — see D6) drives
                        // heading depth when present; every other
                        // protocol collapses "title" to a single `#`,
                        // unchanged from before this fix.
                        let level = match &block.merge_hint {
                            Some(MergeHint::TitleLevel(level)) => (*level).clamp(1, 6),
                            _ => 1,
                        };
                        for _ in 0..level {
                            out.push('#');
                        }
                        out.push(' ');
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

pub fn to_content_list(result: &ParseResult) -> String {
    let items: Vec<serde_json::Value> = result
        .pages
        .iter()
        .flat_map(|page| {
            page.blocks.iter().map(move |block| {
                let mut item = serde_json::json!({
                    "page_num": page.page_num,
                    "category": block.category,
                    "text": block.text,
                    "html": block.html,
                    "latex": block.latex,
                    "asset_path": block.asset_path,
                });
                if let Some(caption) = &block.asset_caption {
                    item.as_object_mut()
                        .expect("content-list item is an object")
                        .insert("asset_caption".to_owned(), serde_json::json!(caption));
                }
                item
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
            route_decision: None,
            preprocess_plan: None,
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
                    asset_caption: None,
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
    fn content_list_includes_asset_caption_only_when_present() {
        let mut result = sample_result();
        result.pages[0].blocks[0].asset_caption = Some(AssetCaption {
            text: "Figure 1: Overview".into(),
            bbox_px: Some([0, 20, 100, 30]),
            confidence: 0.92,
        });
        let list = to_content_list(&result);
        assert!(list.contains("asset_caption"));
        assert!(list.contains("Figure 1: Overview"));

        result.pages[0].blocks[0].asset_caption = None;
        assert!(!to_content_list(&result).contains("asset_caption"));
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
                asset_caption: None,
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
                asset_caption: None,
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

    /// D3: an inline formula (`category == "equation_inline"`, e.g.
    /// pipeline's `inline_formula` regions) renders as `$...$` so it stays
    /// inline with any surrounding text, instead of the unconditional
    /// `$$...$$` that previously split every paragraph containing one into
    /// two blocks. Every other formula category still gets `$$...$$`.
    #[test]
    fn inline_formula_category_renders_as_dollar_delimited_not_display_math() {
        fn formula(category: &str, latex: &str) -> Block {
            Block {
                geom: Geometry::Rect([0.0, 0.0, 10.0, 10.0]),
                geom_frame: CoordFrame::Page,
                bbox_px: Some([0, 0, 10, 10]),
                category_raw: category.into(),
                category: Some(category.into()),
                reading_order: None,
                text: None,
                html: None,
                latex: Some(latex.into()),
                spans: vec![],
                merge_hint: None,
                confidence: None,
                source: BlockSource::LayoutThenRecognize,
                error: None,
                asset_bytes: None,
                asset_path: None,
                asset_caption: None,
            }
        }
        let mut r = sample_result();
        r.pages[0].blocks = vec![formula("equation_inline", "x^2")];
        assert_eq!(to_markdown(&r), "$x^2$");

        r.pages[0].blocks = vec![formula("equation", "x^2")];
        assert_eq!(to_markdown(&r), "$$\nx^2\n$$");
    }

    /// D6: `merge_hint::TitleLevel(n)` drives heading depth for
    /// `category == "title"` blocks when present (currently only set by
    /// the `pipeline` adapter's doc_title/paragraph_title distinction).
    /// Protocols that never set `merge_hint` keep collapsing every title
    /// to a single `#`, unchanged from before this fix.
    #[test]
    fn title_level_merge_hint_drives_heading_depth() {
        fn titled(text: &str, level: Option<u8>) -> Block {
            Block {
                geom: Geometry::Rect([0.0, 0.0, 10.0, 10.0]),
                geom_frame: CoordFrame::Page,
                bbox_px: Some([0, 0, 10, 10]),
                category_raw: "title".into(),
                category: Some("title".into()),
                reading_order: None,
                text: Some(text.into()),
                html: None,
                latex: None,
                spans: vec![],
                merge_hint: level.map(MergeHint::TitleLevel),
                confidence: None,
                source: BlockSource::LayoutThenRecognize,
                error: None,
                asset_bytes: None,
                asset_path: None,
                asset_caption: None,
            }
        }
        let mut r = sample_result();
        r.pages[0].blocks = vec![
            titled("Document Title", Some(1)),
            titled("Section", Some(2)),
            titled("No Hint", None),
        ];
        let md = to_markdown(&r);
        assert!(md.contains("# Document Title"));
        assert!(md.contains("## Section"));
        assert!(md.contains("# No Hint"));
    }
}
