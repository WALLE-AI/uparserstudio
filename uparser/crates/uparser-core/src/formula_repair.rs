//! LaTeX repair chain for formula-block content, per
//! ARCHITECTURE.md §9.2 / T-1.6. Each repairer is an independent,
//! idempotent `fn(&str) -> String`; `repair_chain` runs a configurable
//! sequence. These are generic degenerate-output fixups (per
//! `DEVELOPMENT_PLAN.md`'s literal spec), not a port of MinerU's internal
//! repair functions (whose exact regexes aren't available locally).

use regex::Regex;
use std::sync::LazyLock;

pub type Repairer = fn(&str) -> String;

/// Close any unbalanced `{`/`\left...\right` pairs by appending what's
/// missing at the end.
pub fn balance_brackets(s: &str) -> String {
    let mut out = s.to_string();

    let opens = s.matches('{').count();
    let closes = s.matches('}').count();
    if opens > closes {
        out.push_str(&"}".repeat(opens - closes));
    }

    let left_count = s.matches(r"\left").count();
    let right_count = s.matches(r"\right").count();
    if left_count > right_count {
        // `\right.` is a valid null delimiter — safe filler for any
        // missing `\right`.
        out.push_str(&" \\right.".repeat(left_count - right_count));
    }

    out
}

static REPEATED_QUAD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:\\quad\s*){2,}").expect("valid regex"));

/// Fold runs of repeated `\quad` (a known degenerate-decoding artifact)
/// into a single `\quad`.
pub fn collapse_repeated_quad(s: &str) -> String {
    REPEATED_QUAD_RE
        .replace_all(s, r"\quad ")
        .trim_end()
        .to_string()
}

static EQNO_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\\eqno\s*\(([^)]*)\)").expect("valid regex"));

/// Normalize plain-TeX `\eqno(...)` to amsmath `\tag{...}` for
/// consistency with the rest of the pipeline's tag handling.
pub fn normalize_tag_eqno(s: &str) -> String {
    EQNO_RE.replace_all(s, r"\tag{$1}").to_string()
}

static BEGIN_END_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\\(begin|end)\{([^}]*)\}").expect("valid regex"));

/// Append a matching `\end{env}` for any `\begin{env}` left unclosed
/// (innermost-first, matching proper nesting order).
pub fn rebuild_unclosed_env(s: &str) -> String {
    let mut stack: Vec<&str> = Vec::new();
    for caps in BEGIN_END_RE.captures_iter(s) {
        let kind = &caps[1];
        let env = caps.get(2).unwrap().as_str();
        if kind == "begin" {
            stack.push(env);
        } else if let Some(pos) = stack.iter().rposition(|&e| e == env) {
            stack.remove(pos);
        }
    }

    if stack.is_empty() {
        return s.to_string();
    }

    let mut out = s.to_string();
    for env in stack.iter().rev() {
        out.push_str(&format!("\\end{{{env}}}"));
    }
    out
}

pub const DEFAULT_CHAIN: &[Repairer] = &[
    balance_brackets,
    collapse_repeated_quad,
    normalize_tag_eqno,
    rebuild_unclosed_env,
];

/// Apply a sequence of repairers in order.
pub fn repair_chain(repairers: &[Repairer], input: &str) -> String {
    repairers.iter().fold(input.to_string(), |acc, f| f(&acc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balance_brackets_closes_unclosed_braces() {
        assert_eq!(balance_brackets(r"\frac{1}{2"), r"\frac{1}{2}");
    }

    #[test]
    fn balance_brackets_leaves_balanced_input_untouched() {
        let s = r"\frac{1}{2}";
        assert_eq!(balance_brackets(s), s);
    }

    #[test]
    fn balance_brackets_closes_unmatched_left_right() {
        let out = balance_brackets(r"\left( x + y");
        assert!(out.ends_with(r"\right."));
    }

    #[test]
    fn collapse_repeated_quad_folds_runs() {
        assert_eq!(
            collapse_repeated_quad(r"a \quad \quad \quad b"),
            r"a \quad b"
        );
    }

    #[test]
    fn collapse_repeated_quad_leaves_single_quad_untouched() {
        assert_eq!(collapse_repeated_quad(r"a \quad b"), r"a \quad b");
    }

    #[test]
    fn normalize_tag_eqno_converts_to_tag() {
        assert_eq!(normalize_tag_eqno(r"x = y \eqno(1)"), r"x = y \tag{1}");
    }

    #[test]
    fn normalize_tag_eqno_leaves_tag_untouched() {
        let s = r"x = y \tag{1}";
        assert_eq!(normalize_tag_eqno(s), s);
    }

    #[test]
    fn rebuild_unclosed_env_appends_missing_end() {
        let out = rebuild_unclosed_env(r"\begin{array}{l} a \\ b");
        assert!(out.ends_with(r"\end{array}"));
    }

    #[test]
    fn rebuild_unclosed_env_leaves_closed_env_untouched() {
        let s = r"\begin{array}{l} a \end{array}";
        assert_eq!(rebuild_unclosed_env(s), s);
    }

    #[test]
    fn rebuild_unclosed_env_handles_nesting_order() {
        let out = rebuild_unclosed_env(r"\begin{array}{l}\begin{matrix} a");
        assert!(out.ends_with(r"\end{matrix}\end{array}"));
    }

    #[test]
    fn chain_is_idempotent() {
        let input = r"\begin{array}{l} \left( a \quad \quad b \eqno(2)";
        let once = repair_chain(DEFAULT_CHAIN, input);
        let twice = repair_chain(DEFAULT_CHAIN, &once);
        assert_eq!(once, twice);
    }

    #[test]
    fn chain_leaves_well_formed_formula_untouched() {
        let input = r"\frac{1}{2} + \sqrt{3}";
        assert_eq!(repair_chain(DEFAULT_CHAIN, input), input);
    }

    proptest::proptest! {
        #[test]
        fn arbitrary_input_never_panics(s in ".*") {
            let _ = repair_chain(DEFAULT_CHAIN, &s);
        }
    }
}
