//! NaviDC-OCR's own content post-processing, ported from upstream's
//! `NaviOCR/vlm_utils/post_process/` package.
//!
//! This protocol does **not** share MinerU's repair chain. Upstream runs a
//! different, ordered pipeline over every block before the document is
//! assembled, and the two disagree in ways that change output: upstream
//! wraps display math in `$$…$$` (not `\[…\]`), *deletes* unmatched braces
//! (`try_fix_unbalanced_braces`) where `formula_repair::balance_brackets`
//! *appends* them, and additionally normalizes inline `$…$` in plain text,
//! converts Unicode math to LaTeX, and escapes `%`.
//!
//! Keeping this in its own module (rather than extending
//! `formula_repair.rs`) is deliberate: that chain is shared verbatim by
//! mineru-vlm / dots-ocr / monkeyocr-v2 / pipeline, and changing it to suit
//! one protocol would silently move four others' output.
//!
//! **Not ported** (documented divergence, not an oversight):
//! `equation_left_right.py::try_match_equation_left_right` — 380 lines of
//! array-aware `\left`/`\right` rebalancing that early-returns unchanged
//! whenever the `\left`/`\right` counts already match, so it only fires on
//! malformed formulas. `equation_remove_space.py` is likewise unused by
//! upstream's own `_process_equation` chain.

use regex::Regex;
use std::sync::LazyLock;

/// `UNICODE_TO_LATEX` from `equation_unicode_latex.py` (116 entries).
const UNICODE_TO_LATEX: &[(&str, &str)] = &[
    ("\u{2265}", "\\geq "),
    ("\u{2264}", "\\leq "),
    ("\u{2260}", "\\neq "),
    ("\u{2248}", "\\approx "),
    ("\u{2261}", "\\equiv "),
    ("\u{223c}", "\\sim "),
    ("\u{2243}", "\\simeq "),
    ("\u{2245}", "\\cong "),
    ("\u{221d}", "\\propto "),
    ("\u{226a}", "\\ll "),
    ("\u{226b}", "\\gg "),
    ("\u{227a}", "\\prec "),
    ("\u{227b}", "\\succ "),
    ("\u{227c}", "\\preceq "),
    ("\u{227d}", "\\succeq "),
    ("\u{d7}", "\\times "),
    ("\u{f7}", "\\div "),
    ("\u{b1}", "\\pm "),
    ("\u{2213}", "\\mp "),
    ("\u{b7}", "\\cdot "),
    ("\u{2217}", "\\ast "),
    ("\u{22c6}", "\\star "),
    ("\u{2218}", "\\circ "),
    ("\u{2022}", "\\bullet "),
    ("\u{22c4}", "\\diamond "),
    ("\u{2208}", "\\in "),
    ("\u{2209}", "\\notin "),
    ("\u{220b}", "\\ni "),
    ("\u{2282}", "\\subset "),
    ("\u{2283}", "\\supset "),
    ("\u{2286}", "\\subseteq "),
    ("\u{2287}", "\\supseteq "),
    ("\u{222a}", "\\cup "),
    ("\u{2229}", "\\cap "),
    ("\u{2205}", "\\varnothing "),
    ("\u{2200}", "\\forall "),
    ("\u{2203}", "\\exists "),
    ("\u{ac}", "\\neg "),
    ("\u{2227}", "\\wedge "),
    ("\u{2228}", "\\vee "),
    ("\u{2234}", "\\therefore "),
    ("\u{2235}", "\\because "),
    ("\u{2192}", "\\rightarrow "),
    ("\u{2190}", "\\leftarrow "),
    ("\u{2194}", "\\leftrightarrow "),
    ("\u{21d2}", "\\Rightarrow "),
    ("\u{21d0}", "\\Leftarrow "),
    ("\u{21d4}", "\\Leftrightarrow "),
    ("\u{21a6}", "\\mapsto "),
    ("\u{21aa}", "\\hookrightarrow "),
    ("\u{221e}", "\\infty "),
    ("\u{2202}", "\\partial "),
    ("\u{2207}", "\\nabla "),
    ("\u{2220}", "\\angle "),
    ("\u{25b3}", "\\triangle "),
    ("\u{25a1}", "\\square "),
    ("\u{22a5}", "\\perp "),
    ("\u{2225}", "\\parallel "),
    ("\u{2032}", "\\prime "),
    ("\u{b0}", "\\degree "),
    ("\u{2211}", "\\sum "),
    ("\u{220f}", "\\prod "),
    ("\u{2210}", "\\coprod "),
    ("\u{222b}", "\\int "),
    ("\u{222e}", "\\oint "),
    ("\u{03b1}", "\\alpha "),
    ("\u{03b2}", "\\beta "),
    ("\u{03b3}", "\\gamma "),
    ("\u{03b4}", "\\delta "),
    ("\u{03b5}", "\\epsilon "),
    ("\u{03f5}", "\\varepsilon "),
    ("\u{03b6}", "\\zeta "),
    ("\u{03b7}", "\\eta "),
    ("\u{03b8}", "\\theta "),
    ("\u{03d1}", "\\vartheta "),
    ("\u{03b9}", "\\iota "),
    ("\u{03ba}", "\\kappa "),
    ("\u{03bb}", "\\lambda "),
    ("\u{03bc}", "\\mu "),
    ("\u{03bd}", "\\nu "),
    ("\u{03be}", "\\xi "),
    ("\u{03c0}", "\\pi "),
    ("\u{03d6}", "\\varpi "),
    ("\u{03c1}", "\\rho "),
    ("\u{03f1}", "\\varrho "),
    ("\u{03c3}", "\\sigma "),
    ("\u{03c2}", "\\varsigma "),
    ("\u{03c4}", "\\tau "),
    ("\u{03c5}", "\\upsilon "),
    ("\u{03c6}", "\\phi "),
    ("\u{03d5}", "\\varphi "),
    ("\u{03c7}", "\\chi "),
    ("\u{03c8}", "\\psi "),
    ("\u{03c9}", "\\omega "),
    ("\u{0393}", "\\Gamma "),
    ("\u{0394}", "\\Delta "),
    ("\u{0398}", "\\Theta "),
    ("\u{039b}", "\\Lambda "),
    ("\u{039e}", "\\Xi "),
    ("\u{03a0}", "\\Pi "),
    ("\u{03a3}", "\\Sigma "),
    ("\u{03a5}", "\\Upsilon "),
    ("\u{03a6}", "\\Phi "),
    ("\u{03a8}", "\\Psi "),
    ("\u{03a9}", "\\Omega "),
    ("\u{2135}", "\\aleph "),
    ("\u{210f}", "\\hbar "),
    ("\u{2113}", "\\ell "),
    ("\u{211c}", "\\Re "),
    ("\u{2111}", "\\Im "),
    ("\u{2118}", "\\wp "),
    ("\u{2127}", "\\mho "),
    ("\u{a9}", "\\copyright "),
    ("\u{ae}", "\\registered "),
    ("\u{2020}", "\\dagger "),
    ("\u{2021}", "\\ddagger "),
];

/// `equation_unicode_latex.py::unicode_to_latex` — plain substring
/// replacement, in table order.
pub fn unicode_to_latex(text: &str) -> String {
    let mut out = text.to_string();
    for (uni, latex) in UNICODE_TO_LATEX {
        if out.contains(uni) {
            out = out.replace(uni, latex);
        }
    }
    out
}

static LEADING_MATH_BRACKET: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\$\$)?\s*\\[\[(]\s*").expect("static regex is valid"));
static TRAILING_MATH_BRACKET: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*\\[\])]\s*(\$\$)?$").expect("static regex is valid"));

/// `equation_remove_display_math_brackets.py::remove_math_brackets` —
/// strip a leading `\[`/`\(` and trailing `\]`/`\)`, preserving a `$$`
/// that sits outside them.
pub fn remove_math_brackets(text: &str) -> String {
    let step = LEADING_MATH_BRACKET.replace(text, |c: &regex::Captures| {
        c.get(1).map(|m| m.as_str()).unwrap_or("").to_string()
    });
    TRAILING_MATH_BRACKET
        .replace(&step, |c: &regex::Captures| {
            c.get(1).map(|m| m.as_str()).unwrap_or("").to_string()
        })
        .into_owned()
}

static TAIL_LABEL: LazyLock<Regex> = LazyLock::new(|| {
    // Python uses variable-length lookbehind for `\eqno` with 0-3 trailing
    // spaces, which `regex` cannot express; the `\eqno` guard is applied
    // explicitly on the captured prefix instead (same outcome).
    Regex::new(r"(?s)^(.*?)\s*(\([A-Za-z0-9][A-Za-z0-9.\-]*\))(\s*\$\$\s*)$")
        .expect("static regex is valid")
});
static TAIL_TAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"tag\s*\{([^{}]*)\}(\s*\$\$\s*)$").expect("static regex is valid")
});

/// `equation_double_dollar.py::wrap_with_double_dollar` — strip every `$`,
/// re-wrap in `$$…$$`, then move a trailing `(label)` outside the math and
/// repair a bare `tag{…}` into `\tag{…}`.
pub fn wrap_with_double_dollar(content: &str) -> String {
    let stripped = content.replace('$', "");
    let wrapped = format!("$${}$$", stripped.trim());
    let moved = move_tail_label_after_dollars(&wrapped);
    fix_tail_tag(&moved)
}

fn move_tail_label_after_dollars(text: &str) -> String {
    let Some(caps) = TAIL_LABEL.captures(text) else {
        return text.to_string();
    };
    let prefix = caps.get(1).map(|m| m.as_str()).unwrap_or("");
    // Upstream's lookbehind: a label already introduced by `\eqno` is left
    // alone.
    if prefix.trim_end().ends_with("\\eqno") {
        return text.to_string();
    }
    if !prefix.starts_with("$$") {
        return text.to_string();
    }
    let label = caps.get(2).map(|m| m.as_str()).unwrap_or("");
    let body = prefix[2..].trim_end();
    format!("${body}$ {label}")
}

fn fix_tail_tag(text: &str) -> String {
    let Some(caps) = TAIL_TAG.captures(text) else {
        return text.to_string();
    };
    let whole = caps.get(0).expect("group 0 always present");
    // `(?<!\\)` — skip an already-escaped `\tag`.
    if whole.start() > 0 && text.as_bytes()[whole.start() - 1] == b'\\' {
        return text.to_string();
    }
    let inner = caps.get(1).map(|m| m.as_str()).unwrap_or("");
    let suffix = caps.get(2).map(|m| m.as_str()).unwrap_or("");
    format!("{}\\tag{{{}}}{}", &text[..whole.start()], inner, suffix)
}

/// `equation_unbalanced_braces.py::try_fix_unbalanced_braces` — **delete**
/// every unmatched `{`/`}`, honoring backslash escaping.
///
/// Note the direction: this removes the offending brace, where
/// `formula_repair::balance_brackets` (MinerU's) inserts a matching one.
/// Both are defensible; only one matches this protocol's reference output.
pub fn try_fix_unbalanced_braces(latex: &str) -> String {
    let chars: Vec<char> = latex.chars().collect();
    let mut stack: Vec<usize> = Vec::new();
    let mut unmatched: std::collections::HashSet<usize> = std::collections::HashSet::new();

    for (i, &ch) in chars.iter().enumerate() {
        if ch != '{' && ch != '}' {
            continue;
        }
        let mut backslashes = 0usize;
        let mut j = i;
        while j > 0 && chars[j - 1] == '\\' {
            backslashes += 1;
            j -= 1;
        }
        if backslashes % 2 == 1 {
            continue;
        }
        if ch == '{' {
            stack.push(i);
        } else if stack.pop().is_none() {
            unmatched.insert(i);
        }
    }
    unmatched.extend(stack);
    chars
        .iter()
        .enumerate()
        .filter(|(i, _)| !unmatched.contains(i))
        .map(|(_, c)| *c)
        .collect()
}

/// `equation_escape_latex_special_chars.py` — escape an unescaped `%`.
pub fn escape_latex_special_chars(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    for (i, &ch) in chars.iter().enumerate() {
        if ch == '%' && !(i > 0 && chars[i - 1] == '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// `post_process/__init__.py::_process_equation` — the full ordered chain,
/// minus the deliberately-unported `try_match_equation_left_right` (see the
/// module doc).
pub fn process_equation(content: &str) -> String {
    let s = remove_math_brackets(content);
    let s = wrap_with_double_dollar(&s);
    let s = try_fix_begin_end(&s);
    let s = unicode_to_latex(&s);
    let s = try_fix_unbalanced_braces(&s);
    escape_latex_special_chars(&s)
}

/// `post_process/__init__.py::remove_useless_label` — strip wrapper tags
/// the OTSL converter emits and add the border attribute upstream uses.
pub fn remove_useless_label(html: &str) -> String {
    html.replace("<html>", "")
        .replace("</html>", "")
        .replace("<body>", "")
        .replace("</body>", "")
        .replace("<thead>", "")
        .replace("</thead>", "")
        .replace("<table>", "<table border=\"1\">")
}

/// `equation_fix_begin_end.py::KNOWN_ENVS`.
const KNOWN_ENVS: &[&str] = &[
    "align",
    "align*",
    "aligned",
    "alignedat",
    "array",
    "bmatrix",
    "Bmatrix",
    "pmatrix",
    "vmatrix",
    "Vmatrix",
    "matrix",
    "smallmatrix",
    "cases",
    "dcases",
    "rcases",
    "equation",
    "equation*",
    "gather",
    "gather*",
    "multline",
    "multline*",
    "split",
];

static BEGIN_END_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\\begin\s*\{([^{}]+)\}|\\end\s*\{([^{}]+)\}").expect("static regex is valid")
});

/// `equation_fix_begin_end.py::try_fix_begin_end` — close environments the
/// model left open, and insert the missing `\end{…}` *before* the closing
/// math delimiter rather than after it.
pub fn try_fix_begin_end(latex: &str) -> String {
    let mut stack: Vec<String> = Vec::new();
    let mut result = String::with_capacity(latex.len());
    let mut last = 0usize;

    for m in BEGIN_END_TOKEN.captures_iter(latex) {
        let whole = m.get(0).expect("group 0 always present");
        result.push_str(&latex[last..whole.start()]);
        let token = whole.as_str();

        match (m.get(1), m.get(2)) {
            (Some(begin_env), _) => {
                result.push_str(token);
                let env = begin_env.as_str();
                if KNOWN_ENVS.contains(&env) {
                    stack.push(env.to_string());
                }
            }
            (None, Some(end_env)) => {
                let env = end_env.as_str();
                if !KNOWN_ENVS.contains(&env) {
                    result.push_str(token);
                } else if stack.last().map(String::as_str) == Some(env) {
                    stack.pop();
                    result.push_str(token);
                } else if stack.iter().any(|e| e == env) {
                    // Close the inner environments the model forgot, in
                    // reverse order, before honoring this `\end`.
                    while stack.last().map(String::as_str) != Some(env) {
                        let missing = stack.pop().expect("checked by `any` above");
                        result.push_str(&format!("\\end{{{missing}}}"));
                    }
                    stack.pop();
                    result.push_str(token);
                } else {
                    result.push_str(token);
                }
            }
            _ => result.push_str(token),
        }
        last = whole.end();
    }
    result.push_str(&latex[last..]);

    if stack.is_empty() {
        return result;
    }
    let missing: String = stack
        .iter()
        .rev()
        .map(|env| format!("\\end{{{env}}}"))
        .collect();
    insert_before_math_end(&result, &missing)
}

static MATH_END_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"\$\$\s*$",
        r"\\\]\s*$",
        r"\\end\{equation\*?\}\s*$",
        r"\\end\{align\*?\}\s*$",
    ]
    .iter()
    .map(|p| Regex::new(p).expect("static regex is valid"))
    .collect()
});

fn insert_before_math_end(text: &str, missing: &str) -> String {
    for re in MATH_END_PATTERNS.iter() {
        if let Some(m) = re.find(text) {
            return format!("{}{}{}", &text[..m.start()], missing, &text[m.start()..]);
        }
    }
    format!("{text}{missing}")
}

static INLINE_DOLLAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\$").expect("static regex is valid"));

/// `text_fix_dollar.py::normalize_inline_math` — applied to every **text**
/// block (not just formulas): collapse `$$` to `$`, trim inside each
/// `$…$` pair, and guarantee a space on either side of the pair.
///
/// A text block with an odd number of `$` (or none) is returned untouched,
/// matching upstream — a lone `$` is far more likely to be currency than a
/// broken formula.
pub fn normalize_inline_math(text: &str) -> String {
    let text = text.replace("$$", "$");
    // Python's `(?<!\\)(?<!\$)\$(?!\$)`: after the `$$`→`$` collapse the
    // remaining guards are "not backslash-escaped".
    let bytes = text.as_bytes();
    let positions: Vec<usize> = INLINE_DOLLAR
        .find_iter(&text)
        .map(|m| m.start())
        .filter(|&i| !(i > 0 && bytes[i - 1] == b'\\'))
        .collect();
    if positions.is_empty() || positions.len() % 2 == 1 {
        return text;
    }

    let mut out = String::with_capacity(text.len());
    let mut last = 0usize;
    for pair in positions.chunks_exact(2) {
        let (left, right) = (pair[0], pair[1]);
        out.push_str(&text[last..left]);
        if left > 0 && !text[..left].ends_with(char::is_whitespace) {
            out.push(' ');
        }
        out.push('$');
        out.push_str(text[left + 1..right].trim());
        out.push('$');
        let after = right + 1;
        if after < text.len() && !text[after..].starts_with(char::is_whitespace) {
            out.push(' ');
        }
        last = after;
    }
    out.push_str(&text[last..]);
    out
}

static CODE_LANG_MARKER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^<_([^>]*)_>").expect("static regex is valid"));

/// `guess_suffix_or_lang.py::{code_content_clean, guess_language_by_text}`
/// — the model prefixes recognized code with its own `<_Language_>` marker
/// and may wrap the body in a Markdown fence. Upstream strips both and
/// keeps the language separately; without this the marker leaks verbatim
/// into `Block.text` (confirmed against a real endpoint on the model's own
/// `assets/code.png`, which returns `<_JavaScript_>function canvasApp…`).
///
/// Returns `(language, cleaned_code)`. `language` is `None` when the model
/// emitted no marker — upstream's fallback path there is a `NameError`
/// (`lang` is referenced before assignment), so there is no behavior to
/// match and `DEFAULT_LANG` ("txt") is not invented here.
pub fn strip_code_language_marker(content: &str) -> (Option<String>, String) {
    let cleaned = code_content_clean(content);
    if let Some(caps) = CODE_LANG_MARKER.captures(&cleaned) {
        let lang = caps.get(1).map(|m| m.as_str().to_lowercase());
        let rest = cleaned[caps.get(0).expect("group 0").end()..].to_string();
        return (lang.filter(|l| !l.is_empty()), rest);
    }
    (None, cleaned)
}

/// `guess_suffix_or_lang.py::code_content_clean` — drop a leading fence
/// line and a trailing fence line.
fn code_content_clean(content: &str) -> String {
    if content.is_empty() {
        return String::new();
    }
    let lines: Vec<&str> = content.lines().collect();
    let mut start = 0usize;
    let mut end = lines.len();
    if lines.first().is_some_and(|l| l.starts_with("```")) {
        start = 1;
    }
    if end > start && lines.get(end - 1).is_some_and(|l| l.trim() == "```") {
        end -= 1;
    }
    if start < end {
        lines[start..end].join("\n").trim().to_string()
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Golden values captured by **running upstream's own Python**
    /// (`post_process._process_equation`) on each input, not by reasoning
    /// about what it should do. A divergence here is a divergence from the
    /// reference implementation, which is the whole point of this module.
    #[test]
    fn equation_chain_matches_upstream_python_output() {
        for (input, expected) in [
            (r"\frac{1}{2", r"$$\frac{1}2$$"),
            ("E=mc^2", "$$E=mc^2$$"),
            ("50% x", r"$$50\% x$$"),
            ("a \u{2265} b", r"$$a \geq  b$$"),
            (r"\begin{align}a+b", r"$$\begin{align}a+b\end{align}$$"),
            (r"\[ x+y \]", "$$x+y$$"),
            ("$$E=mc^2(5.3.4-1)$$", "$E=mc^2$ (5.3.4-1)"),
            (
                r"\begin{align}\begin{bmatrix}a&b",
                r"$$\begin{align}\begin{bmatrix}a&b\end{bmatrix}\end{align}$$",
            ),
            // Unmatched braces are *deleted*, not completed.
            ("a}b{c", "$$abc$$"),
        ] {
            assert_eq!(process_equation(input), expected, "input: {input:?}");
        }
    }

    /// `ℝ` has no `UNICODE_TO_LATEX` entry upstream either, so it must
    /// survive untouched — porting a "more complete" table would itself be
    /// a divergence.
    #[test]
    fn unicode_conversion_matches_upstream_including_its_gaps() {
        assert_eq!(
            process_equation("\u{3b1} \u{2192} \u{3b2} \u{2208} \u{211d}"),
            r"$$\alpha  \rightarrow  \beta  \in  \u{211d}$$".replace("\\u{211d}", "\u{211d}")
        );
    }

    #[test]
    fn inline_math_normalization_matches_upstream_python_output() {
        for (input, expected) in [
            ("see $ x + y $ here", "see $x + y$ here"),
            // An odd number of `$` is left alone — currency, not math.
            ("cost is $5", "cost is $5"),
            ("a$b$c", "a $b$ c"),
            ("$$E$$ inline", "$E$ inline"),
            ("no math at all", "no math at all"),
        ] {
            assert_eq!(normalize_inline_math(input), expected, "input: {input:?}");
        }
    }

    #[test]
    fn table_label_cleanup_matches_upstream() {
        assert_eq!(
            remove_useless_label(
                "<html><body><table><thead><tr><td>a</td></tr></thead></table></body></html>"
            ),
            "<table border=\"1\"><tr><td>a</td></tr></table>"
        );
    }

    #[test]
    fn code_language_marker_is_stripped() {
        let (lang, code) = strip_code_language_marker("<_JavaScript_>function f() {}");
        assert_eq!(lang.as_deref(), Some("javascript"));
        assert_eq!(code, "function f() {}");

        // Fenced body, no marker.
        let (lang, code) = strip_code_language_marker("```\nlet x = 1;\n```");
        assert_eq!(lang, None);
        assert_eq!(code, "let x = 1;");
    }

    proptest::proptest! {
        #[test]
        fn equation_chain_never_panics(s in ".*") {
            let _ = process_equation(&s);
        }

        #[test]
        fn inline_math_never_panics(s in ".*") {
            let _ = normalize_inline_math(&s);
        }
    }
}
