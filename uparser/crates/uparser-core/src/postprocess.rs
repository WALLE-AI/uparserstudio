//! Post-processing after protocol adapters emit `Block`s, per
//! ARCHITECTURE.md §4.1 / T-1.10. This pass only implements the
//! pure-geometry layer (proximity/alignment-based paragraph merging with
//! no signal input) — the signal-enhanced layer (using `merge_hint`/
//! `font_size`) is deferred until an adapter that actually emits those
//! signals exists; mineru-vlm (per the P1 plan) turns out to be a
//! pure-geometry consumer itself in this pass, since v0.1.14 doesn't
//! emit `merge_hint`.

use crate::types::{Block, Geometry};

const VERTICAL_GAP_THRESHOLD_PX: i32 = 8;
const HORIZONTAL_ALIGN_TOLERANCE_PX: i32 = 20;

/// Merge adjacent same-category "text" blocks that are vertically close
/// and left-aligned into a single paragraph block, purely from geometry
/// (no `merge_hint` signal). Blocks are consumed and merged in input
/// order — only directly-adjacent list entries are considered, matching
/// typical top-to-bottom layout scan order.
pub fn merge_paragraphs_by_geometry(blocks: Vec<Block>) -> Vec<Block> {
    let mut merged: Vec<Block> = Vec::with_capacity(blocks.len());
    // Index of the last text block emitted, for `MergeHint::SameParagraph`.
    // The signal targets the last *text* block, not the immediately preceding
    // one, because a paragraph continued across a column break can have a
    // figure, a page number or a footer detected between its two halves —
    // this mirrors `json2markdown.py`'s `last_text_contd_idx`.
    let mut last_text: Option<usize> = None;

    for mut block in blocks {
        // Normalize model-generated text (halfwidth/fullwidth punctuation
        // consistency, collapsed whitespace) before merging — merging
        // itself doesn't depend on punctuation, but doing this first
        // means the merged, user-facing output is consistently
        // normalized rather than only the pieces that happened to
        // survive unmerged (see `content_normalize.rs`'s module doc).
        if let Some(text) = &block.text {
            block.text = Some(crate::content_normalize::normalize(text));
        }

        // An explicit signal from the model beats the geometric guess: it is
        // the only thing that can join a paragraph across a column or page
        // break, where the two halves are nowhere near each other.
        if block.merge_hint == Some(crate::types::MergeHint::SameParagraph)
            && let Some(index) = last_text
        {
            let target = &mut merged[index];
            let a = target.bbox_px;
            let b = block.bbox_px;
            merge_into(target, &block, a, b);
            continue;
        }

        if block.category.as_deref() == Some("text")
            && let Some(last) = merged.last_mut()
            && last.category.as_deref() == Some("text")
            && let (Some(a), Some(b)) = (last.bbox_px, block.bbox_px)
        {
            let vertical_gap = b[1] - a[3];
            let left_diff = (a[0] - b[0]).abs();
            if (0..=VERTICAL_GAP_THRESHOLD_PX).contains(&vertical_gap)
                && left_diff <= HORIZONTAL_ALIGN_TOLERANCE_PX
            {
                merge_into(last, &block, Some(a), Some(b));
                continue;
            }
        }
        if block.category.as_deref() == Some("text") {
            last_text = Some(merged.len());
        }
        merged.push(block);
    }

    merged
}

fn merge_into(last: &mut Block, next: &Block, a: Option<[i32; 4]>, b: Option<[i32; 4]>) {
    if let Some(next_text) = &next.text {
        let joined = match &last.text {
            Some(existing) => join_wrapped_lines(existing, next_text),
            None => next_text.clone(),
        };
        // Spans have to follow the text: a consumer rebuilding inline styling
        // checks that the spans still concatenate to the block's text, so
        // merging text without merging spans silently discards every
        // emphasis in the merged paragraph.
        merge_spans(last, next, &joined);
        last.text = Some(joined);
    }
    // A continuation across a column or page break has no meaningful joint
    // bounding box, and either side may carry no geometry at all — keep
    // whichever box exists rather than inventing one.
    let combined_bbox = match (a, b) {
        (Some(a), Some(b)) => Some([
            a[0].min(b[0]),
            a[1].min(b[1]),
            a[2].max(b[2]),
            a[3].max(b[3]),
        ]),
        (Some(a), None) => Some(a),
        (None, b) => b,
    };
    if let Some(bbox) = combined_bbox {
        last.bbox_px = Some(bbox);
        last.geom = Geometry::Rect([
            bbox[0] as f32,
            bbox[1] as f32,
            bbox[2] as f32,
            bbox[3] as f32,
        ]);
    }
}

/// Join two consecutive wrapped lines of the same paragraph with the
/// separator their content actually needs, instead of always inserting
/// an ASCII space (D8's "language-related spacing/hyphenation" item —
/// see `PIPELINE_V2_TABLE_OCR_DEFECT_ANALYSIS.md`). Two real defects
/// this was previously silent about, both mechanical enough to fix
/// without needing real benchmark data to tune a heuristic threshold:
///
/// 1. **CJK line wrap never uses a space.** Chinese/Japanese/Korean
///    prose doesn't put spaces between words — joining two wrapped CJK
///    lines with `format!("{a} {b}")` inserts a visible, incorrect gap
///    (e.g. "…第一条" + "为了…" previously became "…第一条 为了…", not
///    "…第一条为了…"). Detected by checking whether the join point itself
///    (last char of `existing`, first char of `next`) is a Han
///    ideograph, so this doesn't misfire on an English word ending or
///    starting a wrapped line inside an otherwise CJK-dominant document.
/// 2. **End-of-line hyphenation is never undone.** A word broken across
///    a line wrap (`"infor-"` + `"mation"`) previously stayed broken
///    with a space in the middle (`"infor- mation"`) instead of
///    rejoining into `"information"`. Triggers only when the hyphen
///    immediately follows an ASCII letter and the next line starts with
///    a lowercase ASCII letter — deliberately narrow so it doesn't
///    misfire on a genuine trailing "-" (e.g. a bullet marker or a
///    number range) or on an acronym/proper-noun continuation.
/// Concatenate two blocks' spans and reconcile them with the joined text.
///
/// The join can insert a space or drop a hyphen, so the spans are only kept
/// when their concatenation still reproduces the text exactly — otherwise a
/// consumer would map styles onto the wrong characters, which is worse than
/// losing them.
fn merge_spans(last: &mut Block, next: &Block, joined: &str) {
    if last.spans.is_empty() && next.spans.is_empty() {
        return;
    }
    let mut spans = std::mem::take(&mut last.spans);
    let joined_so_far: String = spans.iter().map(|span| span.text.as_str()).collect();
    let separator = joined
        .strip_prefix(joined_so_far.trim_end())
        .map(|rest| rest.len() - rest.trim_start().len())
        .unwrap_or(0);
    if let Some(tail) = spans.last_mut() {
        tail.text = tail.text.trim_end().to_owned();
    }
    if separator > 0 {
        spans.push(crate::types::Span {
            text: " ".repeat(separator),
            bbox_px: None,
            font_size: None,
            is_inline_formula: false,
            style: Default::default(),
        });
    }
    let mut tail = next.spans.clone();
    if let Some(head) = tail.first_mut() {
        head.text = head.text.trim_start().to_owned();
    }
    spans.extend(tail);

    let rebuilt: String = spans.iter().map(|span| span.text.as_str()).collect();
    last.spans = if rebuilt == joined { spans } else { Vec::new() };
}

fn join_wrapped_lines(existing: &str, next: &str) -> String {
    // A line's own text usually carries the trailing space that separated it
    // from the next glyph run; joining without trimming leaves a double space
    // at every wrap, which is visible in the rendered Markdown and costs edit
    // distance on every merged paragraph.
    let existing = existing.trim_end();
    let next = next.trim_start();
    let last_char = existing.chars().next_back();
    let next_first_char = next.chars().next();

    if let (Some(before_hyphen), Some(next_first)) = (
        existing
            .strip_suffix('-')
            .and_then(|s| s.chars().next_back()),
        next_first_char,
    ) && before_hyphen.is_ascii_alphabetic()
        && next_first.is_ascii_lowercase()
    {
        let stripped = &existing[..existing.len() - 1];
        return format!("{stripped}{next}");
    }

    let joins_without_space = matches!(last_char, Some(c) if crate::content_normalize::is_han_ideograph(c))
        || matches!(next_first_char, Some(c) if crate::content_normalize::is_han_ideograph(c));
    if joins_without_space {
        format!("{existing}{next}")
    } else {
        format!("{existing} {next}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{BlockSource, CoordFrame};

    fn text_block(bbox: [i32; 4], text: &str) -> Block {
        Block {
            geom: Geometry::Rect([
                bbox[0] as f32,
                bbox[1] as f32,
                bbox[2] as f32,
                bbox[3] as f32,
            ]),
            geom_frame: CoordFrame::Page,
            bbox_px: Some(bbox),
            category_raw: "text".into(),
            category: Some("text".into()),
            reading_order: None,
            text: Some(text.to_string()),
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

    fn table_block(bbox: [i32; 4]) -> Block {
        let mut b = text_block(bbox, "");
        b.category_raw = "table".into();
        b.category = Some("table".into());
        b.text = None;
        b.html = Some("<table></table>".into());
        b
    }

    /// A line's text keeps the trailing space that separated it from the next
    /// glyph run. Joining without trimming leaves a double space at every
    /// wrap — visible in the output and costly on every merged paragraph.
    /// Merging must carry the spans too, or every emphasis inside a merged
    /// paragraph is lost — the consumer drops spans that no longer
    /// concatenate to the block's text.
    #[test]
    fn merging_carries_spans_so_inline_styles_survive() {
        fn styled(text: &str, italic: bool) -> crate::types::Span {
            crate::types::Span {
                text: text.to_owned(),
                bbox_px: None,
                font_size: None,
                is_inline_formula: false,
                style: crate::types::SpanStyle {
                    italic,
                    ..Default::default()
                },
            }
        }

        let mut first = text_block([0, 0, 100, 10], "GreenComp is");
        first.spans = vec![styled("GreenComp", true), styled(" is", false)];
        let mut second = text_block([0, 12, 100, 22], "a framework");
        second.spans = vec![styled("a framework", false)];

        let merged = merge_paragraphs_by_geometry(vec![first, second]);
        assert_eq!(merged.len(), 1);
        let block = &merged[0];
        assert_eq!(block.text.as_deref(), Some("GreenComp is a framework"));
        let rebuilt: String = block.spans.iter().map(|span| span.text.as_str()).collect();
        assert_eq!(
            rebuilt, "GreenComp is a framework",
            "spans must rebuild the text"
        );
        assert!(block.spans[0].style.italic);
    }

    #[test]
    fn wrapped_lines_join_with_exactly_one_space() {
        let merged = merge_paragraphs_by_geometry(vec![
            text_block([0, 0, 100, 10], "of the identified "),
            text_block([0, 12, 100, 22], " competences, within"),
        ]);
        assert_eq!(merged.len(), 1);
        assert_eq!(
            merged[0].text.as_deref(),
            Some("of the identified competences, within")
        );
    }

    #[test]
    fn merges_close_aligned_text_blocks() {
        let blocks = vec![
            text_block([10, 0, 200, 20], "First line."),
            text_block([10, 25, 200, 45], "Second line."),
        ];
        let merged = merge_paragraphs_by_geometry(blocks);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text.as_deref(), Some("First line. Second line."));
        assert_eq!(merged[0].bbox_px, Some([10, 0, 200, 45]));
    }

    #[test]
    fn normalizes_punctuation_before_merging() {
        // The real case content_normalize.rs exists for: adjacent
        // blocks with inconsistent halfwidth/fullwidth punctuation
        // should come out normalized after this pass, not just merged
        // as-is (D.10-adjacent — see the "P0：后处理模块强化" section
        // of CLI_ENHANCEMENT_PROPOSAL.md).
        let blocks = vec![text_block([10, 0, 200, 20], "安全投入符合安全生产要求;")];
        let merged = merge_paragraphs_by_geometry(blocks);
        assert_eq!(
            merged[0].text.as_deref(),
            Some("安全投入符合安全生产要求；")
        );
    }

    #[test]
    fn does_not_normalize_punctuation_in_non_cjk_text() {
        let blocks = vec![text_block([10, 0, 200, 20], "Hello, world; how are you?")];
        let merged = merge_paragraphs_by_geometry(blocks);
        assert_eq!(
            merged[0].text.as_deref(),
            Some("Hello, world; how are you?")
        );
    }

    #[test]
    fn does_not_merge_across_large_vertical_gap() {
        let blocks = vec![
            text_block([10, 0, 200, 20], "First."),
            text_block([10, 500, 200, 520], "Far away."),
        ];
        let merged = merge_paragraphs_by_geometry(blocks);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn does_not_merge_misaligned_columns() {
        let blocks = vec![
            text_block([10, 0, 200, 20], "Left column."),
            text_block([300, 25, 500, 45], "Right column."),
        ];
        let merged = merge_paragraphs_by_geometry(blocks);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn never_merges_non_text_categories() {
        let blocks = vec![
            table_block([10, 0, 200, 20]),
            table_block([10, 25, 200, 45]),
        ];
        let merged = merge_paragraphs_by_geometry(blocks);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn single_block_passthrough() {
        let blocks = vec![text_block([0, 0, 10, 10], "solo")];
        let merged = merge_paragraphs_by_geometry(blocks);
        assert_eq!(merged.len(), 1);
    }

    #[test]
    fn empty_input_yields_empty_output() {
        assert!(merge_paragraphs_by_geometry(vec![]).is_empty());
    }

    #[test]
    fn chained_merge_of_three_lines() {
        let blocks = vec![
            text_block([10, 0, 200, 20], "One"),
            text_block([10, 25, 200, 45], "Two"),
            text_block([10, 50, 200, 70], "Three"),
        ];
        let merged = merge_paragraphs_by_geometry(blocks);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text.as_deref(), Some("One Two Three"));
    }

    /// D8: merging two wrapped CJK lines must not insert an ASCII space
    /// between them — Chinese prose doesn't use spaces between words,
    /// and a visible gap in the merged output is a real, user-visible
    /// defect distinct from `content_normalize.rs`'s punctuation fix.
    #[test]
    fn merges_cjk_lines_without_inserting_a_space() {
        let blocks = vec![
            text_block([10, 0, 200, 20], "安全生产许可证条例第一条"),
            text_block([10, 25, 200, 45], "为了加强安全生产许可管理"),
        ];
        let merged = merge_paragraphs_by_geometry(blocks);
        assert_eq!(merged.len(), 1);
        assert_eq!(
            merged[0].text.as_deref(),
            Some("安全生产许可证条例第一条为了加强安全生产许可管理")
        );
    }

    /// D8: a word broken across a line wrap by a hyphen (e.g. from a
    /// justified English PDF) should rejoin into the real word, not stay
    /// split with a space in the middle.
    #[test]
    fn dehyphenates_a_word_broken_across_a_line_wrap() {
        let blocks = vec![
            text_block([10, 0, 200, 20], "This is an infor-"),
            text_block([10, 25, 200, 45], "mation retrieval system."),
        ];
        let merged = merge_paragraphs_by_geometry(blocks);
        assert_eq!(merged.len(), 1);
        assert_eq!(
            merged[0].text.as_deref(),
            Some("This is an information retrieval system.")
        );
    }

    /// D8: a genuine trailing hyphen that isn't a line-wrap break (e.g.
    /// followed by a capitalized word/number, or a bullet-style dash)
    /// must not be silently dehyphenated.
    #[test]
    fn does_not_dehyphenate_when_the_next_line_does_not_look_like_a_word_continuation() {
        let blocks = vec![
            text_block([10, 0, 200, 20], "See appendix A-"),
            text_block([10, 25, 200, 45], "1 for details."),
        ];
        let merged = merge_paragraphs_by_geometry(blocks);
        assert_eq!(merged.len(), 1);
        assert_eq!(
            merged[0].text.as_deref(),
            Some("See appendix A- 1 for details.")
        );
    }

    /// D8: ordinary English word-wrap (no hyphen, no CJK) keeps the
    /// existing single-space join — this fix must not regress the
    /// already-correct default case.
    #[test]
    fn ordinary_latin_line_wrap_still_joins_with_a_single_space() {
        let blocks = vec![
            text_block([10, 0, 200, 20], "First line."),
            text_block([10, 25, 200, 45], "Second line."),
        ];
        let merged = merge_paragraphs_by_geometry(blocks);
        assert_eq!(merged[0].text.as_deref(), Some("First line. Second line."));
    }

    /// Gate G2 proof: this module is unmodified by P2, and must treat a
    /// dots.ocr-shaped block list (different `source`/`category_raw`
    /// casing convention, `reading_order` populated, still no
    /// `merge_hint`) exactly the same as the mineru-vlm-shaped blocks
    /// tested above — same merge outcome for the same geometry,
    /// regardless of which protocol produced the blocks.
    #[test]
    fn merges_dots_ocr_shaped_blocks_identically_to_mineru_shaped_blocks() {
        fn dots_ocr_text_block(bbox: [i32; 4], text: &str, order: u32) -> Block {
            let mut b = text_block(bbox, text);
            b.category_raw = "Text".into();
            b.source = BlockSource::OneShotVlm;
            b.reading_order = Some(order);
            b
        }

        let mineru_shaped = vec![
            text_block([10, 0, 200, 20], "First line."),
            text_block([10, 25, 200, 45], "Second line."),
        ];
        let dots_ocr_shaped = vec![
            dots_ocr_text_block([10, 0, 200, 20], "First line.", 0),
            dots_ocr_text_block([10, 25, 200, 45], "Second line.", 1),
        ];

        let merged_mineru = merge_paragraphs_by_geometry(mineru_shaped);
        let merged_dots_ocr = merge_paragraphs_by_geometry(dots_ocr_shaped);

        assert_eq!(merged_mineru.len(), 1);
        assert_eq!(merged_dots_ocr.len(), 1);
        assert_eq!(merged_mineru[0].text, merged_dots_ocr[0].text);
        assert_eq!(merged_mineru[0].bbox_px, merged_dots_ocr[0].bbox_px);
    }

    /// Gate G6 proof (T-6.3): a `paddleocr`-shaped block set carries
    /// `Geometry::Polygon` (not `Rect`) geometry, but this module never
    /// reads `geom` directly — only `bbox_px` (the polygon's bounding
    /// rect, computed once by the adapter via `geometry::geometry_bounds`
    /// per `reading_order.rs`'s design note) drives the merge decision.
    /// Confirms polygon input doesn't require any special-casing here.
    #[test]
    fn merges_polygon_shaped_paddleocr_blocks_via_their_bounding_rect() {
        fn polygon_text_block(polygon: Vec<[f32; 2]>, bbox: [i32; 4], text: &str) -> Block {
            let mut b = text_block(bbox, text);
            b.geom = Geometry::Polygon(polygon);
            b.category_raw = String::new();
            b.source = BlockSource::OcrPipeline;
            b
        }

        let blocks = vec![
            polygon_text_block(
                vec![[10.0, 0.0], [200.0, 0.0], [200.0, 20.0], [10.0, 20.0]],
                [10, 0, 200, 20],
                "First line.",
            ),
            polygon_text_block(
                vec![[10.0, 25.0], [200.0, 25.0], [200.0, 45.0], [10.0, 45.0]],
                [10, 25, 200, 45],
                "Second line.",
            ),
        ];

        let merged = merge_paragraphs_by_geometry(blocks);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text.as_deref(), Some("First line. Second line."));
        assert_eq!(merged[0].bbox_px, Some([10, 0, 200, 45]));
        // The merge always writes a Rect back (postprocess.rs's own
        // combined-bbox representation) — the original polygon detail is
        // rightly lost on merge, but nothing panicked or mis-happened
        // consuming the polygon-sourced input.
        assert!(matches!(merged[0].geom, Geometry::Rect(_)));
    }
}
