//! VLM-output text normalization — model-generated text quality issues
//! that live purely at the character level, distinct from structural
//! fixups in `output_parse.rs`/`otsl.rs`/`formula_repair.rs`.

use std::sync::LazyLock;

/// Convert fullwidth ASCII letters and digits to halfwidth ASCII. OmniDocBench
/// removes punctuation for text edit distance, but it does not NFKC-normalize
/// letters/digits, so this aligns the cheap character-level part of MinerU's
/// `full_to_half_exclude_marks` behavior without rewriting punctuation.
pub fn normalize_fullwidth_alnum(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        let mapped = match c {
            'Ａ'..='Ｚ' => char::from_u32(c as u32 - 'Ａ' as u32 + 'A' as u32),
            'ａ'..='ｚ' => char::from_u32(c as u32 - 'ａ' as u32 + 'a' as u32),
            '０'..='９' => char::from_u32(c as u32 - '０' as u32 + '0' as u32),
            _ => None,
        };
        out.push(mapped.unwrap_or(c));
    }
    out
}

static HORIZONTAL_WS_RUN_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"[ \t]{2,}").expect("valid regex"));
static NEWLINE_RUN_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\n{2,}").expect("valid regex"));

/// Collapse runs of repeated spaces/tabs into a single space, and runs
/// of repeated newlines into a single newline — a VLM occasionally emits
/// extra whitespace with no semantic meaning behind it.
pub fn collapse_whitespace(text: &str) -> String {
    let collapsed = HORIZONTAL_WS_RUN_RE.replace_all(text, " ");
    NEWLINE_RUN_RE.replace_all(&collapsed, "\n").into_owned()
}

/// The full normalization pipeline applied to a block's `text` before
/// document-level postprocessing merges it with its neighbors.
pub fn normalize(text: &str) -> String {
    collapse_whitespace(&normalize_fullwidth_alnum(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_fullwidth_letters_and_digits() {
        assert_eq!(
            normalize_fullwidth_alnum("ＡＢＣ ａｂｃ １２３"),
            "ABC abc 123"
        );
    }

    #[test]
    fn leaves_punctuation_width_untouched() {
        let s = "安全投入符合安全生产要求;（第１条）";
        assert_eq!(
            normalize_fullwidth_alnum(s),
            "安全投入符合安全生产要求;（第1条）"
        );
    }

    #[test]
    fn collapse_whitespace_folds_repeated_spaces() {
        assert_eq!(collapse_whitespace("a   b"), "a b");
    }

    #[test]
    fn collapse_whitespace_folds_repeated_newlines() {
        assert_eq!(collapse_whitespace("a\n\n\n\nb"), "a\nb");
    }

    #[test]
    fn collapse_whitespace_leaves_single_space_and_newline_untouched() {
        assert_eq!(collapse_whitespace("a b\nc"), "a b\nc");
    }

    #[test]
    fn normalize_composes_both_steps() {
        assert_eq!(normalize("编号Ａ１２   value"), "编号A12 value");
    }

    proptest::proptest! {
        #[test]
        fn arbitrary_input_never_panics(s in ".*") {
            let _ = normalize(&s);
        }
    }
}
