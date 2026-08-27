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
                merge_into(last, &block, a, b);
                continue;
            }
        }
        merged.push(block);
    }

    merged
}

fn merge_into(last: &mut Block, next: &Block, a: [i32; 4], b: [i32; 4]) {
    if let Some(next_text) = &next.text {
        last.text = Some(match &last.text {
            Some(existing) => join_wrapped_lines(existing, next_text),
            None => next_text.clone(),
        });
    }
    let combined_bbox = [
        a[0].min(b[0]),
        a[1].min(b[1]),
        a[2].max(b[2]),
        a[3].max(b[3]),
    ];
    last.bbox_px = Some(combined_bbox);
    last.geom = Geometry::Rect([
        combined_bbox[0] as f32,
        combined_bbox[1] as f32,
        combined_bbox[2] as f32,
        combined_bbox[3] as f32,
    ]);
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
fn join_wrapped_lines(existing: &str, next: &str) -> String {
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
