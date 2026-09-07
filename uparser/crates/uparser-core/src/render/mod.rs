//! Render a `ParseResult` to an output format. P0 covers markdown/json/
//! content-list only, without table HTML embedding or other advanced
//! formatting (those land alongside the adapters that produce the
//! richer signals they depend on).

use crate::types::ParseResult;

/// Which producer supplies Markdown.
///
/// Only one distinction survives: whether the **native PDF engine's own**
/// Markdown is used, or whether that document is rendered from the canonical
/// model like everything else. Model protocols and structured sources have a
/// single renderer either way.
///
/// `Engine` remains the default because the vendored engine's Markdown is
/// what the published native benchmark score was measured on, and rendering
/// a PDF from the IR still loses signals the engine only expresses in text
/// (see the plan's §15).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkdownSource {
    Canonical,
    EngineLegacy,
}

/// Everything a completed run can be asked to render.
pub struct RenderInput<'a> {
    pub result: &'a ParseResult,
    /// The engine's own Markdown, when the protocol produced one.
    pub engine_markdown: Option<&'a str>,
    /// The canonical document, when the source was a structured format.
    pub document: Option<&'a uparser_document_engine::CanonicalDocument>,
    /// Format to record on a document ascended from `result`.
    pub source_format: uparser_document_engine::DocumentFormat,
}

#[derive(Debug)]
pub enum RenderError {
    Serialization(String),
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenderError::Serialization(message) => write!(formatter, "{message}"),
        }
    }
}

/// The one place an output format is turned into bytes (O5.4).
///
/// This used to be a four-way `if/else` inline in `cli.rs`, which meant the
/// library API and the CLI could disagree about what `--format markdown`
/// means, and that `document-json` was reachable only for structured
/// sources. Both are decided here now.
pub fn render_markdown(input: &RenderInput<'_>, source: MarkdownSource) -> String {
    if source == MarkdownSource::EngineLegacy
        && let Some(markdown) = input.engine_markdown
    {
        return markdown.to_owned();
    }
    // The canonical renderer terminates its output with a newline; the CLI
    // adds one of its own when writing the line. Trim so the emitted file
    // does not gain a trailing blank line just because of which renderer
    // produced it.
    let rendered = match input.document {
        // A structured source already *is* a canonical document; ascending
        // its own lowered blocks would only lose what it already has.
        Some(document) => uparser_document_engine::render::markdown(document),
        // Everything else — every model protocol, and `pipeline` — is lifted
        // into the canonical model and rendered by the one renderer. This is
        // the merge: the `Page`/`Block` Markdown writer that used to serve
        // these protocols is gone, so escaping, list indentation and table
        // degradation are decided in exactly one place.
        None => uparser_document_engine::render::markdown(&crate::ascend::to_canonical_document(
            input.result,
            input.source_format,
        )),
    };
    rendered.trim_end().to_owned()
}

/// `--format document-json` for any protocol: a structured source renders
/// its own document, everything else renders one ascended from the IR.
pub fn render_document_json(input: &RenderInput<'_>) -> Result<String, RenderError> {
    let owned;
    let document = match input.document {
        Some(document) => document,
        None => {
            owned = crate::ascend::to_canonical_document(input.result, input.source_format);
            &owned
        }
    };
    uparser_document_engine::render::document_json(document)
        .map_err(|error| RenderError::Serialization(error.to_string()))
}

pub fn to_json(result: &ParseResult) -> String {
    serde_json::to_string_pretty(result).expect("ParseResult is always serializable")
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

    /// Render through the one renderer, the way every caller now does.
    fn markdown(result: &ParseResult) -> String {
        render_markdown(
            &RenderInput {
                result,
                engine_markdown: None,
                document: None,
                source_format: uparser_document_engine::DocumentFormat::Pdf,
            },
            MarkdownSource::Canonical,
        )
    }

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
        insta::assert_snapshot!(markdown(&sample_result()));
    }

    #[test]
    fn markdown_renders_an_image_only_block_as_a_link() {
        let mut result = sample_result();
        result.pages[0].blocks[0].text = None;
        result.pages[0].blocks[0].asset_path = Some("doc_images/abc123.png".into());
        assert_eq!(markdown(&result), "![](doc_images/abc123.png)");
    }

    #[test]
    fn markdown_prefers_text_over_asset_path_when_both_are_present() {
        // Shouldn't happen in practice (an adapter either extracts text
        // or crops an image, not both, for the same block) but the
        // fallback order should still be deterministic if it ever does.
        let mut result = sample_result();
        result.pages[0].blocks[0].asset_path = Some("doc_images/abc123.png".into());
        assert_eq!(markdown(&result), "Hello world");
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

        assert_eq!(markdown(&mineru_shaped), markdown(&dots_ocr_shaped));
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
        let md = markdown(&r);
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
        assert_eq!(markdown(&r), "$x^2$");

        r.pages[0].blocks = vec![formula("equation", "x^2")];
        assert_eq!(markdown(&r), "$$\nx^2\n$$");
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
        let md = markdown(&r);
        assert!(md.contains("# Document Title"));
        assert!(md.contains("## Section"));
        assert!(md.contains("# No Hint"));
    }
}
