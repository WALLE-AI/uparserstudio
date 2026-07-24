//! Render a `ParseResult` to an output format. P0 covers markdown/json/
//! content-list only, without table HTML embedding or other advanced
//! formatting (those land alongside the adapters that produce the
//! richer signals they depend on).

use crate::types::ParseResult;

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
                out.push_str("$$\n");
                out.push_str(latex);
                out.push_str("\n$$\n\n");
            } else if let Some(text) = &block.text {
                out.push_str(text);
                out.push_str("\n\n");
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
                serde_json::json!({
                    "page_num": page.page_num,
                    "category": block.category,
                    "text": block.text,
                    "html": block.html,
                    "latex": block.latex,
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
}
