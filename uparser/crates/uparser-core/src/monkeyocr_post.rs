//! MonkeyOCRv2's own content post-processing, ported from
//! `opensource/MonkeyOCRv2/parsing/core_runner.py`.
//!
//! Like `navidc_post`, this exists because the shared MinerU-derived
//! helpers disagree with this protocol's reference implementation in ways
//! that change output:
//!
//! - `formula_repair::collapse_repeated_quad` folds runs of **2 or more**
//!   `\quad`; upstream folds **5 or more**, and also folds `\qquad`, which
//!   the shared helper does not handle at all. Folding a legitimate
//!   two-`\quad` spacing is a silent content change.
//! - Upstream extracts a trailing equation label (`\tag{…}`, `\eqno(…)`,
//!   `\quad(…)`) *out* of the math and re-appends it after the closing
//!   `$$`; the shared chain's `normalize_tag_eqno` instead rewrites
//!   `\eqno` into `\tag{}` and leaves it inside.
//! - Upstream's `otsl_to_html` treats `xcel` as a plain skipped cell,
//!   where `otsl::to_html` resolves it against its up/left neighbours
//!   (this project's own D.6 change). Different tables come out.
//!
//! Changing the shared modules to match would silently move mineru-vlm /
//! dots-ocr / pipeline output, so the divergence lives here instead.

use regex::Regex;
use std::sync::LazyLock;

static REPEATED_QUAD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:\\quad\s*){5,}").expect("static regex is valid"));
static REPEATED_QQUAD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:\\qquad\s*){5,}").expect("static regex is valid"));
/// `(?:\quad|\qquad|\eqno)\s*\(([^()]*)\)\s*$ | \tag\{([^{}]*)\}\s*$`
static TRAILING_LABEL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:\\quad|\\qquad|\\eqno)\s*\(([^()]*)\)\s*$|\\tag\{([^{}]*)\}\s*$")
        .expect("static regex is valid")
});
static LEADING_BEGIN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\\begin\{([^}]+)\}").expect("static regex is valid"));

fn take_trailing_label(content: &str) -> (String, Option<String>) {
    let Some(m) = TRAILING_LABEL.captures(content) else {
        return (content.to_string(), None);
    };
    // Upstream reads `match.group(1)` unconditionally, so a `\tag{…}` match
    // (which only fills group 2) yields `None` — reproduced, not "fixed".
    let extracted = m.get(1).map(|g| g.as_str().to_string());
    let whole = m.get(0).expect("group 0 always present");
    (content[..whole.start()].trim_end().to_string(), extracted)
}

/// `core_runner.py::process_formula` — returns `(content, label)`.
pub fn process_formula(content: &str) -> (String, Option<String>) {
    let content = content.trim_matches('$').trim();
    let content = REPEATED_QUAD.replace_all(content, r"\quad ");
    let content = REPEATED_QQUAD.replace_all(&content, r"\qquad ");
    let content = content.trim();

    let (mut content, mut extracted) = take_trailing_label(content);

    // Strip a leading `\begin{env}` and its matching trailing `\end{env}`,
    // so a label sitting *inside* the environment can be pulled out too.
    let mut begin_env: Option<String> = None;
    if let Some(bm) = LEADING_BEGIN.captures(&content) {
        let env = bm.get(1).expect("group 1").as_str().to_string();
        let after = bm.get(0).expect("group 0").end();
        content = content[after..].trim_start().to_string();
        let end_re = Regex::new(&format!(r"\\end\{{{}\}}\s*$", regex::escape(&env)))
            .expect("env name is escaped");
        if let Some(em) = end_re.find(&content) {
            content = content[..em.start()].trim_end().to_string();
        }
        begin_env = Some(env);
    }

    let (stripped, second) = take_trailing_label(&content);
    if second.is_some() || TRAILING_LABEL.is_match(&content) {
        content = stripped;
        if second.is_some() {
            extracted = second;
        }
    }

    if let Some(env) = begin_env {
        content = format!("\\begin{{{env}}}\n{content}\n\\end{{{env}}}");
    }
    (content, extracted)
}

/// `_format_block_fields`'s `Formula` branch: `$$\n{content}\n$$\n`, then
/// the extracted label appended *outside* the math.
pub fn format_formula(raw: &str) -> String {
    let (content, extracted) = process_formula(raw);
    let mut out = format!("$$\n{content}\n$$\n");
    if let Some(label) = extracted {
        out.push_str(&label);
    }
    out
}

/// `result2md` strips the Unicode replacement character from the finished
/// Markdown ("Remove invalid characters").
pub fn strip_replacement_chars(s: &str) -> String {
    s.replace('\u{fffd}', "")
}

#[derive(Debug, Clone)]
struct Cell {
    text: String,
    rowspan: usize,
    colspan: usize,
    valid: bool,
}

/// Upstream uses `<([a-z]+)>(.*?)(?=<[a-z]+>|$)`; the `regex` crate has no
/// lookahead, so the equivalent is done by locating each tag and slicing
/// up to the next one.
static OTSL_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<([a-z]+)>").expect("static regex is valid"));

/// `(tag, content)` pairs, content running to the next tag or end of row.
fn otsl_tokens(row: &str) -> Vec<(&str, &str)> {
    let marks: Vec<(usize, usize, &str)> = OTSL_TAG
        .captures_iter(row)
        .map(|c| {
            let whole = c.get(0).expect("group 0");
            let tag = c.get(1).expect("group 1").as_str();
            (whole.start(), whole.end(), tag)
        })
        .collect();
    marks
        .iter()
        .enumerate()
        .map(|(i, &(_, end, tag))| {
            let next = marks.get(i + 1).map(|m| m.0).unwrap_or(row.len());
            (tag, &row[end..next])
        })
        .collect()
}

/// `core_runner.py::otsl_to_html` — grid reconstruction.
///
/// Deliberately *not* `otsl::to_html`: upstream resolves `lcel`/`ucel` by
/// walking left/up to the nearest **valid** cell and extending its span,
/// and treats `xcel` as a skipped placeholder that extends nothing. The
/// shared implementation resolves `xcel` against both neighbours instead.
pub fn otsl_to_html(otsl: &str) -> String {
    if otsl.trim().is_empty() {
        return "<table></table>".to_string();
    }
    let mut rows_tokens: Vec<&str> = otsl.split("<nl>").collect();
    if rows_tokens.last() == Some(&"") {
        rows_tokens.pop();
    }

    let mut grid: Vec<Vec<Option<Cell>>> = Vec::new();
    for (r_idx, row_str) in rows_tokens.iter().enumerate() {
        if r_idx >= grid.len() {
            grid.push(Vec::new());
        }
        if row_str.trim().is_empty() {
            continue;
        }
        let mut col_idx = 0usize;
        for (tag, content) in otsl_tokens(row_str) {
            // Advance past already-occupied columns.
            loop {
                while grid[r_idx].len() <= col_idx {
                    grid[r_idx].push(None);
                }
                if grid[r_idx][col_idx].is_some() {
                    col_idx += 1;
                } else {
                    break;
                }
            }
            match tag {
                "fcel" | "ecel" => {
                    let text = if tag == "fcel" {
                        content.trim().to_string()
                    } else {
                        String::new()
                    };
                    grid[r_idx][col_idx] = Some(Cell {
                        text,
                        rowspan: 1,
                        colspan: 1,
                        valid: true,
                    });
                    col_idx += 1;
                }
                "lcel" => {
                    let mut found = false;
                    let mut search_c = col_idx as isize - 1;
                    while search_c >= 0 {
                        let c = search_c as usize;
                        if let Some(Some(cell)) = grid[r_idx].get_mut(c)
                            && cell.valid
                        {
                            cell.colspan += 1;
                            found = true;
                            break;
                        }
                        search_c -= 1;
                    }
                    grid[r_idx][col_idx] = Some(placeholder_or_cell(found));
                    col_idx += 1;
                }
                "ucel" => {
                    let mut found = false;
                    let mut search_r = r_idx as isize - 1;
                    while search_r >= 0 {
                        let r = search_r as usize;
                        if let Some(Some(cell)) = grid[r].get_mut(col_idx)
                            && cell.valid
                        {
                            cell.rowspan += 1;
                            found = true;
                            break;
                        }
                        search_r -= 1;
                    }
                    grid[r_idx][col_idx] = Some(placeholder_or_cell(found));
                    col_idx += 1;
                }
                "xcel" => {
                    grid[r_idx][col_idx] = Some(placeholder_or_cell(true));
                    col_idx += 1;
                }
                _ => col_idx += 1,
            }
        }
    }

    let mut html = String::from("<table>");
    for row in &grid {
        html.push_str("<tr>");
        for cell in row.iter().flatten() {
            if !cell.valid {
                continue;
            }
            let mut attrs = String::new();
            if cell.rowspan > 1 {
                attrs.push_str(&format!(" rowspan=\"{}\"", cell.rowspan));
            }
            if cell.colspan > 1 {
                attrs.push_str(&format!(" colspan=\"{}\"", cell.colspan));
            }
            let text = cell
                .text
                .replace("\r\n", "\n")
                .replace('\r', "\n")
                .split('\n')
                .map(crate::otsl::escape_html)
                .collect::<Vec<_>>()
                .join("<br>");
            html.push_str(&format!("<td{attrs}>{text}</td>"));
        }
        html.push_str("</tr>");
    }
    html.push_str("</table>");
    html
}

fn placeholder_or_cell(found: bool) -> Cell {
    if found {
        Cell {
            text: String::new(),
            rowspan: 1,
            colspan: 1,
            valid: false,
        }
    } else {
        Cell {
            text: String::new(),
            rowspan: 1,
            colspan: 1,
            valid: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Golden values captured by **executing upstream's own
    /// `core_runner.py::process_formula`**, not by reasoning about it.
    #[test]
    fn process_formula_matches_upstream_python_output() {
        for (input, content, label) in [
            (r"\frac{1}{2}", r"\frac{1}{2}", None),
            // Upstream reads `match.group(1)` unconditionally, so a
            // `\tag{…}` (group 2) yields no label even though it *is*
            // stripped from the content. Reproduced deliberately.
            (r"E=mc^2\tag{3.1}", "E=mc^2", None),
            (r"x+y \eqno(1-2)", "x+y", Some("1-2")),
            // Threshold is 5, not 2: three `\quad` survive untouched.
            (r"\quad \quad \quad a", r"\quad \quad \quad a", None),
            (r"\quad \quad \quad \quad \quad \quad a", r"\quad a", None),
            (
                r"\begin{align}a+b\end{align}\tag{9}",
                "\\begin{align}\na+b\n\\end{align}",
                None,
            ),
            ("$$z$$", "z", None),
        ] {
            let (got_content, got_label) = process_formula(input);
            assert_eq!(got_content, content, "content for {input:?}");
            assert_eq!(got_label.as_deref(), label, "label for {input:?}");
        }
    }

    /// Same discipline for the table converter. Note the `xcel` row: it
    /// extends nothing and simply disappears, unlike `otsl::to_html`.
    #[test]
    fn otsl_to_html_matches_upstream_python_output() {
        for (input, expected) in [
            (
                "<fcel>a<fcel>b<nl><fcel>c<fcel>d<nl>",
                "<table><tr><td>a</td><td>b</td></tr><tr><td>c</td><td>d</td></tr></table>",
            ),
            (
                "<fcel>a<lcel><nl><fcel>c<fcel>d<nl>",
                "<table><tr><td colspan=\"2\">a</td></tr><tr><td>c</td><td>d</td></tr></table>",
            ),
            (
                "<fcel>a<fcel>b<nl><ucel><fcel>d<nl>",
                "<table><tr><td rowspan=\"2\">a</td><td>b</td></tr><tr><td>d</td></tr></table>",
            ),
            (
                "<fcel>a<fcel>b<nl><xcel><fcel>d<nl>",
                "<table><tr><td>a</td><td>b</td></tr><tr><td>d</td></tr></table>",
            ),
            ("<fcel>a&b<nl>", "<table><tr><td>a&amp;b</td></tr></table>"),
        ] {
            assert_eq!(otsl_to_html(input), expected, "input: {input:?}");
        }
    }

    #[test]
    fn empty_otsl_is_an_empty_table() {
        assert_eq!(otsl_to_html("   "), "<table></table>");
    }

    #[test]
    fn formula_is_wrapped_and_label_moved_outside_the_math() {
        assert_eq!(format_formula(r"x+y \eqno(1-2)"), "$$\nx+y\n$$\n1-2");
        assert_eq!(format_formula("a=b"), "$$\na=b\n$$\n");
    }

    #[test]
    fn replacement_characters_are_stripped() {
        assert_eq!(strip_replacement_chars("a\u{fffd}b"), "ab");
    }

    proptest::proptest! {
        #[test]
        fn process_formula_never_panics(s in ".*") {
            let _ = process_formula(&s);
        }

        #[test]
        fn otsl_to_html_never_panics(s in ".*") {
            let _ = otsl_to_html(&s);
        }
    }
}
