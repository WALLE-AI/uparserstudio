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
//!
//! `"list_item"` was added to mineru-vlm's vocab post-P1, confirmed via
//! a live run against a real `MinerU2.5-2604-1.2B` vLLM endpoint (not
//! present in the v0.1.14 `mineru_vl_utils` vocab this module was
//! originally reverse-engineered from — the served checkpoint is newer
//! than that package version, same version-drift caveat noted in
//! `adapters/mineru_vlm.rs`'s module doc).
//!
//! `"image_block"` is a composite-image parent emitted by
//! `MinerU2.5-Pro-2605`; it contains one or more `image` child regions.

/// mineru-vlm's native category vocabulary (lowercased, per the wire
/// format's own lowercasing in `output_parse::parse_custom_tokens`).
pub const MINERU_VLM_CATEGORIES: &[&str] = &[
    "text",
    "title",
    "table",
    "image",
    "image_block",
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
    "list_item",
    "phonetic",
    "table_caption",
    "image_caption",
    "code_caption",
    "table_footnote",
    "image_footnote",
    "unknown",
];

/// Normalize a raw category label before matching: lowercase and strip
/// hyphens/underscores/whitespace, so vocabulary drift like `"Text"` vs
/// `"text"` or `"List-item"` vs `"list_item"` still resolves instead of
/// silently falling into the `"unknown"` branch. Previously only
/// `map_mineru_vlm_category` was effectively normalized (because its
/// caller, `output_parse::parse_custom_tokens`, already lowercases the
/// raw category upstream) while `map_dots_ocr_category`/
/// `map_monkeyocrv2_category`/`map_pipeline_category` matched the exact
/// native spelling with no defense at all — `list_item`'s real vocab
/// drift on mineru-vlm was only caught because it happened to land in
/// the one mapper with any normalization; the other three protocols had
/// no equivalent safety net (see D.8 in `CLI_ENHANCEMENT_PROPOSAL.md`).
fn normalize_key(raw: &str) -> String {
    raw.trim()
        .to_lowercase()
        .chars()
        .filter(|c| !matches!(c, '-' | '_' | ' '))
        .collect()
}

/// Map a mineru-vlm native category to this project's normalized
/// category string. Unrecognized input falls back to `"unknown"` with a
/// warning rather than failing.
pub fn map_mineru_vlm_category(raw: &str) -> (String, Option<String>) {
    let normalized = match normalize_key(raw).as_str() {
        "text" | "asidetext" | "phonetic" => "text",
        "title" => "title",
        "table" => "table",
        "image" | "imageblock" => "image",
        "code" | "algorithm" => "code",
        "header" => "header",
        "footer" => "footer",
        "pagenumber" => "page_number",
        "pagefootnote" | "tablefootnote" | "imagefootnote" => "footnote",
        "equation" | "equationblock" => "equation",
        "reftext" => "reference",
        "list" | "listitem" => "list",
        "tablecaption" | "imagecaption" | "codecaption" => "caption",
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
    let normalized = match normalize_key(raw).as_str() {
        "caption" => "caption",
        "footnote" => "footnote",
        "formula" => "equation",
        "listitem" => "list",
        "pagefooter" => "footer",
        "pageheader" => "header",
        "picture" => "image",
        "sectionheader" => "title",
        "table" => "table",
        "text" => "text",
        "title" => "title",
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
    let normalized = match normalize_key(raw).as_str() {
        "caption" => "caption",
        "listitem" => "list",
        "pagefooter" => "footer",
        "pageheader" => "header",
        "sectionheader" => "title",
        "text" => "text",
        "title" => "title",
        "formula" => "equation",
        "table" => "table",
        "picture" => "image",
        _ => {
            return (
                "unknown".to_string(),
                Some(format!("unrecognized MonkeyOCRv2 category: {raw:?}")),
            );
        }
    };
    (normalized.to_string(), None)
}

/// `pipeline` protocol's native layout-stage category vocabulary.
///
/// **Correction**: this constant was originally documented as "confirmed
/// from `enum_class.py`'s `BlockType` — the 'Added in pp_doclayout_v2'
/// subset", which is misleading — that `BlockType` subset (`abstract`,
/// `doc_title`, `paragraph_title`, `vertical_text`, `header_image`,
/// `footer_image`, `formula_number`) is MinerU's own *post-processed*
/// semantic vocabulary, not the raw model output. The real PP-DocLayoutV2
/// model's native label set (`PP_DOCLAYOUT_V2_LABELS` in
/// `opensource/MinerU/mineru/model/layout/pp_doclayoutv2.py`) is a third,
/// larger, mostly-disjoint 25-label vocabulary (`algorithm`,
/// `aside_text`, `content`, `number`, `reference`, `reference_content`,
/// `seal`, etc., alongside labels this constant already lists). See
/// `PIPELINE_V2_TABLE_OCR_DEFECT_ANALYSIS.md`'s S3 notes.
/// `map_pipeline_category` below accepts all three vocabularies (this
/// constant's, the legacy PDF-Extract-Kit one, and the real
/// PP-DocLayoutV2 one) so it works unmodified regardless of which model
/// chain the standalone stage endpoints are currently running.
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
///
/// The `pipeline` adapter's standalone stage endpoints currently run the
/// legacy-compatible model chain (`legacy_backends.py`/`compat.py`),
/// whose label set (`LEGACY_LAYOUT_LABELS`/`LEGACY_MFD_LABELS`) is a
/// PDF-Extract-Kit vocabulary, not `PIPELINE_LAYOUT_CATEGORIES`' native
/// PP-DocLayoutV2 vocabulary — the two never agreed, so
/// `display_formula`/`inline_formula`/`figure_title`/`vision_footnote`/
/// `formula_number` all fell through to `unknown` (see
/// `PIPELINE_V2_TABLE_OCR_DEFECT_ANALYSIS.md`, D2). Both vocabularies are
/// accepted here so this mapper works unmodified once the standalone
/// endpoints are upgraded to the PP-DocLayoutV2 model chain.
///
/// `"inline_formula"` maps to a distinct `"equation_inline"` category
/// (not `"equation"`) so the renderer can emit `$...$` instead of
/// `$$...$$` for it — see `render::to_markdown` and D3.
///
/// The real PP-DocLayoutV2 additions (`algorithm`, `aside_text`,
/// `content`, `footer_image`, `header_image`, `number`, `reference`,
/// `reference_content`, `seal`) are mapped per MinerU's own ground-truth
/// translation table, `PP_DOCLAYOUT_V2_LABELS_TO_BLOCK_TYPES` in
/// `opensource/MinerU/mineru/backend/pipeline/pipeline_magic_model.py` —
/// read directly, not guessed. That table has no entry for bare
/// `"reference"` (only `"reference_content"`) — real MinerU appears to
/// treat the `"reference"` list-outer-frame region as a non-content
/// container, so it's mapped to `"discarded"` here rather than invented.
pub fn map_pipeline_category(raw: &str) -> (String, Option<String>) {
    let normalized = match normalize_key(raw).as_str() {
        "doctitle" | "paragraphtitle" | "verticaltext" => "title",
        "text" | "abstract" | "asidetext" => "text",
        "image" | "seal" => "image",
        "table" => "table",
        "chart" => "chart",
        "interlineequation" | "displayformula" => "equation",
        "inlineformula" => "equation_inline",
        "list" => "list",
        "index" | "content" => "index",
        "header" | "headerimage" => "header",
        "footer" | "footerimage" => "footer",
        "pagenumber" | "number" => "page_number",
        "footnote" | "visionfootnote" => "footnote",
        "figuretitle" => "caption",
        "formulanumber" => "text",
        "algorithm" => "code",
        "referencecontent" => "reference",
        "discarded" | "reference" => "discarded",
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

    /// Confirmed via a live run against a real `MinerU2.5-2604-1.2B`
    /// endpoint — this checkpoint emits `list_item` (not in the v0.1.14
    /// `mineru_vl_utils` vocab this module was originally built against).
    #[test]
    fn list_item_maps_to_list_same_as_list() {
        assert_eq!(map_mineru_vlm_category("list_item").0, "list");
        assert_eq!(
            map_mineru_vlm_category("list_item").0,
            map_mineru_vlm_category("list").0
        );
    }

    #[test]
    fn equation_and_equation_block_both_map_to_equation() {
        assert_eq!(map_mineru_vlm_category("equation").0, "equation");
        assert_eq!(map_mineru_vlm_category("equation_block").0, "equation");
    }

    #[test]
    fn image_block_maps_to_image_without_warning() {
        let (normalized, warning) = map_mineru_vlm_category("image_block");
        assert_eq!(normalized, "image");
        assert!(warning.is_none());
    }

    #[test]
    fn dots_ocr_category_matching_is_case_and_hyphen_insensitive() {
        // Simulates the kind of vocabulary drift a model update could
        // introduce (e.g. lowercase or underscore instead of the
        // documented Title-Case/hyphen spelling) — previously only
        // mineru-vlm's mapper had any normalization defense (see D.8).
        assert_eq!(map_dots_ocr_category("text").0, "text");
        assert_eq!(map_dots_ocr_category("TEXT").0, "text");
        assert_eq!(map_dots_ocr_category("list_item").0, "list");
        assert_eq!(map_dots_ocr_category("List Item").0, "list");
    }

    #[test]
    fn monkeyocrv2_category_matching_is_case_and_hyphen_insensitive() {
        assert_eq!(map_monkeyocrv2_category("text").0, "text");
        assert_eq!(map_monkeyocrv2_category("LIST-ITEM").0, "list");
        assert_eq!(map_monkeyocrv2_category("List_Item").0, "list");
    }

    #[test]
    fn pipeline_category_matching_is_case_and_hyphen_insensitive() {
        assert_eq!(map_pipeline_category("PAGE_NUMBER").0, "page_number");
        assert_eq!(map_pipeline_category("Interline-Equation").0, "equation");
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

    /// D2: the standalone `pipeline` stage endpoints currently run the
    /// legacy-compatible model chain, whose label set
    /// (`compat.py::LEGACY_LAYOUT_LABELS`/`LEGACY_MFD_LABELS`) is disjoint
    /// from `PIPELINE_LAYOUT_CATEGORIES`. Every legacy label must map to a
    /// known category without warning.
    #[test]
    fn pipeline_category_accepts_the_legacy_backend_label_set() {
        for raw in [
            "paragraph_title",
            "text",
            "discarded",
            "image",
            "figure_title",
            "table",
            "vision_footnote",
            "display_formula",
            "formula_number",
            "inline_formula",
        ] {
            let (normalized, warning) = map_pipeline_category(raw);
            assert_ne!(
                normalized, "unknown",
                "legacy label {raw} mapped to unknown"
            );
            assert!(warning.is_none(), "legacy label {raw} should not warn");
        }
    }

    #[test]
    fn pipeline_inline_formula_is_distinct_from_block_formula() {
        assert_eq!(map_pipeline_category("inline_formula").0, "equation_inline");
        assert_eq!(map_pipeline_category("display_formula").0, "equation");
        assert_eq!(map_pipeline_category("interline_equation").0, "equation");
    }

    /// S3: `Mineru345LayoutBackend` (the real PP-DocLayoutV2 model) emits
    /// the full native `PP_DOCLAYOUT_V2_LABELS` vocabulary, a third,
    /// mostly-disjoint set from both the legacy vocabulary and this
    /// module's own `PIPELINE_LAYOUT_CATEGORIES` constant — every label
    /// in it must map to something known.
    #[test]
    fn pipeline_category_accepts_the_real_pp_doclayout_v2_label_set() {
        for raw in [
            "abstract",
            "algorithm",
            "aside_text",
            "chart",
            "content",
            "display_formula",
            "doc_title",
            "figure_title",
            "footer",
            "footer_image",
            "footnote",
            "formula_number",
            "header",
            "header_image",
            "image",
            "inline_formula",
            "number",
            "paragraph_title",
            "reference",
            "reference_content",
            "seal",
            "table",
            "text",
            "vertical_text",
            "vision_footnote",
        ] {
            let (normalized, warning) = map_pipeline_category(raw);
            assert_ne!(
                normalized, "unknown",
                "real PP-DocLayoutV2 label {raw} mapped to unknown"
            );
            assert!(warning.is_none(), "real label {raw} should not warn");
        }
    }
}
