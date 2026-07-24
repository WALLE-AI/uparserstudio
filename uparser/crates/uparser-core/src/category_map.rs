//! Native protocol category → this project's normalized category
//! vocabulary, per ARCHITECTURE.md §9.2. P1 covers mineru-vlm's 21
//! native categories (confirmed from `mineru_vl_utils` v0.1.14's
//! `BlockType`/`BLOCK_TYPES`); P2 (T-2.4) adds dots.ocr's 11 native
//! Title-Case categories (confirmed from vendored
//! `opensource/dots.ocr/dots_ocr/utils/prompts.py`); P3 (T-3.4) adds
//! MonkeyOCRv2's 10 native Title-Case categories (confirmed from
//! vendored `opensource/MonkeyOCRv2/parsing/core_runner.py`) — all three
//! map into the same normalized vocabulary, demonstrating this module is
//! a per-protocol function registry, not single-protocol logic.

/// mineru-vlm's native category vocabulary (lowercased, per the wire
/// format's own lowercasing in `output_parse::parse_custom_tokens`).
pub const MINERU_VLM_CATEGORIES: &[&str] = &[
    "text",
    "title",
    "table",
    "image",
    "code",
    "algorithm",
    "header",
    "footer",
    "page_number",
    "page_footnote",
    "aside_text",
    "equation",
    "equation_block",
    "ref_text",
    "list",
    "phonetic",
    "table_caption",
    "image_caption",
    "code_caption",
    "table_footnote",
    "image_footnote",
    "unknown",
];

/// Map a mineru-vlm native category to this project's normalized
/// category string. Unrecognized input falls back to `"unknown"` with a
/// warning rather than failing.
pub fn map_mineru_vlm_category(raw: &str) -> (String, Option<String>) {
    let normalized = match raw {
        "text" | "aside_text" | "phonetic" => "text",
        "title" => "title",
        "table" => "table",
        "image" => "image",
        "code" | "algorithm" => "code",
        "header" => "header",
        "footer" => "footer",
        "page_number" => "page_number",
        "page_footnote" | "table_footnote" | "image_footnote" => "footnote",
        "equation" | "equation_block" => "equation",
        "ref_text" => "reference",
        "list" => "list",
        "table_caption" | "image_caption" | "code_caption" => "caption",
        "unknown" => "unknown",
        _ => {
            return (
                "unknown".to_string(),
                Some(format!("unrecognized mineru-vlm category: {raw:?}")),
            );
        }
    };
    (normalized.to_string(), None)
}

/// dots.ocr's native category vocabulary (Title Case, per
/// `prompt_layout_all_en` in `prompts.py`).
pub const DOTS_OCR_CATEGORIES: &[&str] = &[
    "Caption",
    "Footnote",
    "Formula",
    "List-item",
    "Page-footer",
    "Page-header",
    "Picture",
    "Section-header",
    "Table",
    "Text",
    "Title",
];

/// Map a dots.ocr native category to this project's normalized category
/// string. Unrecognized input falls back to `"unknown"` with a warning.
pub fn map_dots_ocr_category(raw: &str) -> (String, Option<String>) {
    let normalized = match raw {
        "Caption" => "caption",
        "Footnote" => "footnote",
        "Formula" => "equation",
        "List-item" => "list",
        "Page-footer" => "footer",
        "Page-header" => "header",
        "Picture" => "image",
        "Section-header" => "title",
        "Table" => "table",
        "Text" => "text",
        "Title" => "title",
        _ => {
            return (
                "unknown".to_string(),
                Some(format!("unrecognized dots.ocr category: {raw:?}")),
            );
        }
    };
    (normalized.to_string(), None)
}

/// MonkeyOCRv2's native category vocabulary (Title Case, per
/// `ALL_PROMPT` keys + `Picture` in `core_runner.py`; `Footnote` is
/// present in source but commented out/disabled, so excluded here).
pub const MONKEYOCR_V2_CATEGORIES: &[&str] = &[
    "Caption",
    "List-item",
    "Page-footer",
    "Page-header",
    "Section-header",
    "Text",
    "Title",
    "Formula",
    "Table",
    "Picture",
];

/// Map a MonkeyOCRv2 native category to this project's normalized
/// category string. Unrecognized input falls back to `"unknown"` with a
/// warning.
pub fn map_monkeyocrv2_category(raw: &str) -> (String, Option<String>) {
    let normalized = match raw {
        "Caption" => "caption",
        "List-item" => "list",
        "Page-footer" => "footer",
        "Page-header" => "header",
        "Section-header" => "title",
        "Text" => "text",
        "Title" => "title",
        "Formula" => "equation",
        "Table" => "table",
        "Picture" => "image",
        _ => {
            return (
                "unknown".to_string(),
                Some(format!("unrecognized MonkeyOCRv2 category: {raw:?}")),
            );
        }
    };
    (normalized.to_string(), None)
}

/// `pipeline` protocol's native layout-stage category vocabulary,
/// confirmed from `opensource/MinerU/mineru/utils/enum_class.py`'s
/// `BlockType` — specifically the "Added in pp_doclayout_v2" subset,
/// since PP-DocLayoutV2 is the pipeline's sole layout model (P5).
pub const PIPELINE_LAYOUT_CATEGORIES: &[&str] = &[
    "doc_title",
    "paragraph_title",
    "text",
    "abstract",
    "image",
    "table",
    "chart",
    "interline_equation",
    "list",
    "index",
    "header",
    "footer",
    "page_number",
    "footnote",
    "vertical_text",
    "discarded",
];

/// Map a `pipeline` native layout category to this project's normalized
/// category string. Unrecognized input falls back to `"unknown"` with a
/// warning.
pub fn map_pipeline_category(raw: &str) -> (String, Option<String>) {
    let normalized = match raw {
        "doc_title" | "paragraph_title" | "vertical_text" => "title",
        "text" | "abstract" => "text",
        "image" => "image",
        "table" => "table",
        "chart" => "chart",
        "interline_equation" => "equation",
        "list" => "list",
        "index" => "index",
        "header" => "header",
        "footer" => "footer",
        "page_number" => "page_number",
        "footnote" => "footnote",
        "discarded" => "discarded",
        _ => {
            return (
                "unknown".to_string(),
                Some(format!("unrecognized pipeline layout category: {raw:?}")),
            );
        }
    };
    (normalized.to_string(), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_monkeyocrv2_category_maps_to_something_non_empty() {
        for &raw in MONKEYOCR_V2_CATEGORIES {
            let (normalized, warning) = map_monkeyocrv2_category(raw);
            assert!(
                !normalized.is_empty(),
                "category {raw} mapped to empty string"
            );
            assert!(warning.is_none(), "known category {raw} should not warn");
        }
    }

    #[test]
    fn monkeyocrv2_unrecognized_category_falls_back_to_unknown_with_warning() {
        let (normalized, warning) = map_monkeyocrv2_category("Footnote");
        assert_eq!(normalized, "unknown");
        assert!(warning.is_some());
    }

    #[test]
    fn every_dots_ocr_category_maps_to_something_non_empty() {
        for &raw in DOTS_OCR_CATEGORIES {
            let (normalized, warning) = map_dots_ocr_category(raw);
            assert!(
                !normalized.is_empty(),
                "category {raw} mapped to empty string"
            );
            assert!(warning.is_none(), "known category {raw} should not warn");
        }
    }

    #[test]
    fn dots_ocr_unrecognized_category_falls_back_to_unknown_with_warning() {
        let (normalized, warning) = map_dots_ocr_category("Chart");
        assert_eq!(normalized, "unknown");
        assert!(warning.is_some());
    }

    #[test]
    fn every_native_category_maps_to_something_non_empty() {
        for &raw in MINERU_VLM_CATEGORIES {
            let (normalized, warning) = map_mineru_vlm_category(raw);
            assert!(
                !normalized.is_empty(),
                "category {raw} mapped to empty string"
            );
            assert!(warning.is_none(), "known category {raw} should not warn");
        }
    }

    #[test]
    fn unrecognized_category_falls_back_to_unknown_with_warning() {
        let (normalized, warning) = map_mineru_vlm_category("chart");
        assert_eq!(normalized, "unknown");
        assert!(warning.is_some());
    }

    #[test]
    fn captions_are_grouped_together() {
        assert_eq!(map_mineru_vlm_category("table_caption").0, "caption");
        assert_eq!(map_mineru_vlm_category("image_caption").0, "caption");
        assert_eq!(map_mineru_vlm_category("code_caption").0, "caption");
    }

    #[test]
    fn equation_and_equation_block_both_map_to_equation() {
        assert_eq!(map_mineru_vlm_category("equation").0, "equation");
        assert_eq!(map_mineru_vlm_category("equation_block").0, "equation");
    }

    #[test]
    fn every_pipeline_category_maps_to_something_non_empty() {
        for &raw in PIPELINE_LAYOUT_CATEGORIES {
            let (normalized, warning) = map_pipeline_category(raw);
            assert!(
                !normalized.is_empty(),
                "category {raw} mapped to empty string"
            );
            assert!(warning.is_none(), "known category {raw} should not warn");
        }
    }

    #[test]
    fn pipeline_unrecognized_category_falls_back_to_unknown_with_warning() {
        let (normalized, warning) = map_pipeline_category("watermark");
        assert_eq!(normalized, "unknown");
        assert!(warning.is_some());
    }
}
