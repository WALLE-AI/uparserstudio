//! VLM-output text normalization — model-generated text quality issues
//! that live purely at the *character* level, distinct from the
//! structural-level fixups `output_parse.rs`/`otsl.rs`/`formula_repair.rs`
//! already handle. Motivated by a real case found in this project's own
//! live-endpoint output: adjacent list items in the same document mixed
//! halfwidth and fullwidth punctuation (`;` vs `；`) with no consistency,
//! because a VLM's training data contains both forms and it doesn't
//! reliably pick one per document (see the "P0：后处理模块强化" section
//! of `CLI_ENHANCEMENT_PROPOSAL.md`).

use std::sync::LazyLock;

/// Below this fraction of CJK ideographs among non-whitespace
/// characters, `normalize_punctuation` leaves the text untouched — the
/// single biggest risk of this function is rewriting legitimate
/// halfwidth punctuation in English text, code blocks, or formulas.
const CJK_RATIO_THRESHOLD: f64 = 0.2;

fn is_han_ideograph(c: char) -> bool {
    matches!(c as u32, 0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0xF900..=0xFAFF)
}

fn is_cjk_dominant(text: &str) -> bool {
    let total = text.chars().filter(|c| !c.is_whitespace()).count();
    if total == 0 {
        return false;
    }
    let cjk = text.chars().filter(|&c| is_han_ideograph(c)).count();
    (cjk as f64 / total as f64) >= CJK_RATIO_THRESHOLD
}

/// Unify halfwidth punctuation (`, . ; : ? ! ( )`) to its fullwidth
/// counterpart, but only when the surrounding text is CJK-dominant —
/// otherwise this would corrupt legitimate halfwidth punctuation in
/// English/code/formula content. A `.` between two ASCII digits (e.g.
/// `3.14`) is deliberately left alone even in CJK-dominant text, since
/// that's almost always a decimal number, not a Chinese sentence-ending
/// full stop.
pub fn normalize_punctuation(text: &str) -> String {
    if !is_cjk_dominant(text) {
        return text.to_string();
    }

    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    for (i, &c) in chars.iter().enumerate() {
        let mapped = match c {
            ',' => Some('，'),
            ';' => Some('；'),
            ':' => Some('：'),
            '?' => Some('？'),
            '!' => Some('！'),
            '(' => Some('（'),
            ')' => Some('）'),
            '.' => {
                let prev_digit = i > 0 && chars[i - 1].is_ascii_digit();
                let next_digit = i + 1 < chars.len() && chars[i + 1].is_ascii_digit();
                if prev_digit && next_digit {
                    None
                } else {
                    Some('。')
                }
            }
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
    collapse_whitespace(&normalize_punctuation(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_halfwidth_punctuation_in_cjk_dominant_text() {
        // The real case this module exists for.
        assert_eq!(
            normalize_punctuation("安全投入符合安全生产要求;"),
            "安全投入符合安全生产要求；"
        );
    }

    #[test]
    fn leaves_halfwidth_punctuation_untouched_in_english_text() {
        let s = "The quick brown fox; it jumps (over) the dog: really!";
        assert_eq!(normalize_punctuation(s), s);
    }

    #[test]
    fn leaves_halfwidth_punctuation_untouched_in_code_like_content() {
        let s = "fn main() { println!(\"hi\"); }";
        assert_eq!(normalize_punctuation(s), s);
    }

    #[test]
    fn does_not_touch_decimal_points_even_in_cjk_dominant_text() {
        let s = "价格是3.14元，请确认。";
        assert_eq!(normalize_punctuation(s), s);
    }

    #[test]
    fn converts_a_real_sentence_ending_period_in_cjk_text() {
        let s = "这是一句话.";
        assert_eq!(normalize_punctuation(s), "这是一句话。");
    }

    #[test]
    fn mixed_cjk_and_english_above_threshold_still_normalizes() {
        // Mostly Chinese with an embedded English word — still
        // CJK-dominant overall, so punctuation should normalize.
        let s = "这是一个test句子;";
        assert_eq!(normalize_punctuation(s), "这是一个test句子；");
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
        assert_eq!(
            normalize("安全生产要求;   还有更多要求;"),
            "安全生产要求； 还有更多要求；"
        );
    }

    proptest::proptest! {
        #[test]
        fn arbitrary_input_never_panics(s in ".*") {
            let _ = normalize(&s);
        }
    }
}
