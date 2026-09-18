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

/// **Only** the six OTSL control tags are tokens. Upstream's regex is
/// `<(fcel|ecel|lcel|ucel|xcel|nl)>(.*?)(?=<(?:…)>|\Z)` with
/// `DOTALL | IGNORECASE`, and its own comment says why the whitelist
/// matters: "Other markup (e.g. `<br>` or a nested `<table>`) is cell
/// content and must remain untouched."
///
/// This adapter previously used `<([a-z]+)>`, which swallowed *any*
/// lowercase tag — so a single `<br>` inside a cell was consumed as a
/// control token and shifted every following cell of the table one column
/// left. That silently wrecked the grid of any table whose cells contain
/// line breaks or inline markup.
///
/// The `regex` crate has no lookahead, so the content slice is taken by
/// locating each tag and cutting at the next one.
static OTSL_TAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)<(fcel|ecel|lcel|ucel|xcel|nl)>").expect("static regex is valid")
});

/// `<otsl>…</otsl>` wrapper, stripped recursively before tokenization.
static OTSL_WRAPPER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<otsl>(.*?)</otsl>").expect("static regex is valid"));
static OTSL_WRAPPER_OPEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<otsl>").expect("static regex is valid"));

/// `html2otsl`'s private escape for literal control-looking tags: the
/// sentinel is U+E100, and `\u{e100}fcel>` means a literal `<fcel>` that
/// is cell *content*, not a control token.
static OTSL_PRIVATE_ESCAPE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\u{e100}(fcel|ecel|lcel|ucel|xcel|nl)>").expect("static regex is valid")
});

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

    // An `<otsl>`-wrapped payload is unwrapped and converted recursively.
    // Upstream checks for the *opening* tag but substitutes on the full
    // open/close pair, so an unclosed `<otsl>` falls through with the
    // literal tag still in the string — reproduced, not "fixed".
    if OTSL_WRAPPER_OPEN.is_match(otsl) {
        return OTSL_WRAPPER
            .replace_all(otsl, |caps: &regex::Captures<'_>| {
                otsl_to_html(caps.get(1).expect("group 1").as_str())
            })
            .into_owned();
    }

    // Case-sensitive, matching upstream's `otsl_str.split("<nl>")`. A
    // literal `<NL>` therefore stays inside its row, gets tokenized by the
    // case-insensitive regex, and is then dropped by the case-sensitive
    // tag dispatch below.
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
            // Upstream tokenizes case-insensitively but dispatches with
            // `tag == 'fcel'` etc., so an uppercase `<FCEL>` matches the
            // regex and then falls through to the final `else`: the column
            // advances and no cell is emitted.
            match tag {
                "fcel" | "ecel" => {
                    // Cell content is serialized HTML from `html2otsl`.
                    // Upstream's comment — "Do not normalize whitespace" —
                    // is load-bearing: this used to `.trim()`, which is a
                    // silent content change for any cell whose leading or
                    // trailing space is meaningful.
                    let text = if tag == "fcel" {
                        let decoded = content.replace("\u{e100}\u{e100}", "\u{e100}");
                        OTSL_PRIVATE_ESCAPE
                            .replace_all(&decoded, "<$1>")
                            .into_owned()
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
            let raw_text = cell.text.replace("\r\n", "\n").replace('\r', "\n");
            let text = render_cell_text(&raw_text);
            html.push_str(&format!("<td{attrs}>{text}</td>"));
        }
        html.push_str("</tr>");
    }
    html.push_str("</table>");
    html
}

/// An opening HTML tag inside cell content: `re.split(r'(<[A-Za-z][^>]*>)')`.
/// Note it requires a **letter** right after `<`, so a *closing* tag like
/// `</b>` does not match and ends up escaped as text. That is upstream's
/// behavior (verified by running it: `<b>bold</b>` renders as
/// `<b>bold&lt;/b&gt;`), so it is reproduced rather than corrected.
static CELL_HTML_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<[A-Za-z][^>]*>").expect("static regex is valid"));
static CELL_BR_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^<br\s*/?>$").expect("static regex is valid"));
/// A `&` that already opens a character reference must not be re-escaped.
/// Upstream expresses this as the negative lookahead
/// `&(?!(?:#\d+|#x[0-9a-fA-F]+|[A-Za-z][A-Za-z0-9]+);)`; the `regex` crate
/// has no lookahead, so the same test is applied to the remainder.
static CELL_ENTITY_AT_START: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:#[0-9]+|#x[0-9a-fA-F]+|[A-Za-z][A-Za-z0-9]+);").expect("static regex is valid")
});

/// Escape a plain-text chunk of cell content the way upstream does:
/// entity-preserving `&`, then `<`/`>`/`"`/`'`, then newline → `<br>`.
///
/// Deliberately **not** `otsl::escape_html` — that one escapes `&`
/// unconditionally (turning an already-serialized `&amp;` into
/// `&amp;amp;`) and leaves `"`/`'` alone. Changing the shared helper would
/// move mineru-vlm / dots-ocr / pipeline table output, so the divergence
/// lives here.
fn escape_cell_chunk(chunk: &str) -> String {
    let mut out = String::with_capacity(chunk.len());
    let mut rest = chunk;
    while let Some(idx) = rest.find('&') {
        out.push_str(&rest[..idx]);
        let after = &rest[idx + 1..];
        if CELL_ENTITY_AT_START.is_match(after) {
            out.push('&');
        } else {
            out.push_str("&amp;");
        }
        rest = after;
    }
    out.push_str(rest);

    out.replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
        .replace('\n', "<br>")
}

/// `otsl_to_html`'s `<td>` body: embedded HTML tags pass through (with
/// `<br/>` normalized to `<br>`), plain text is escaped.
fn render_cell_text(raw_text: &str) -> String {
    let mut out = String::new();
    let mut last_end = 0usize;
    for m in CELL_HTML_TAG.find_iter(raw_text) {
        out.push_str(&escape_cell_chunk(&raw_text[last_end..m.start()]));
        let tag = m.as_str();
        if CELL_BR_TAG.is_match(tag) {
            out.push_str("<br>");
        } else {
            out.push_str(tag);
        }
        last_end = m.end();
    }
    out.push_str(&escape_cell_chunk(&raw_text[last_end..]));
    out
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

/// `[img][x1,y1,x2,y2][/img]` — a picture embedded *inside* a table, which
/// the model emits as a marker carrying the sub-region's `[0,1000]`-space
/// coordinates relative to the table crop.
static TABLE_IMAGE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\[img\]\s*\[\s*(-?\d+(?:\.\d+)?)\s*,\s*(-?\d+(?:\.\d+)?)\s*,\s*(-?\d+(?:\.\d+)?)\s*,\s*(-?\d+(?:\.\d+)?)\s*\]\s*\[/img\]",
    )
    .expect("static regex is valid")
});

/// `core_runner.py::_replace_table_image_markers`.
///
/// Upstream offers two output shapes for the cropped sub-image: a file
/// written to `image_dir`, or an inline base64 data URI (`use_base64`).
/// This port takes the **base64 branch** deliberately: a single table can
/// carry several embedded pictures, and `Block` has room for exactly one
/// `asset_bytes`/`asset_path`, so the file branch cannot be expressed
/// without an IR change. The base64 branch is a first-class upstream
/// option, so this is a configuration choice rather than a divergence.
///
/// Returns `content` untouched when there is no marker at all — same
/// early-out as upstream.
pub fn replace_table_image_markers(content: &str, table_image: &image::RgbImage) -> String {
    if !TABLE_IMAGE_PATTERN.is_match(content) {
        return content.to_string();
    }
    let (width, height) = table_image.dimensions();
    // Upstream caches by normalized bbox, so the same region is cropped
    // and encoded once however many times it is referenced.
    let mut refs: std::collections::HashMap<[String; 4], String> = std::collections::HashMap::new();

    let mut out = String::new();
    let mut last_end = 0usize;
    for caps in TABLE_IMAGE_PATTERN.captures_iter(content) {
        let whole = caps.get(0).expect("group 0");
        out.push_str(&content[last_end..whole.start()]);
        last_end = whole.end();

        let raw: [String; 4] = std::array::from_fn(|i| {
            caps.get(i + 1)
                .expect("four coordinate groups")
                .as_str()
                .to_string()
        });
        let image_ref = match refs.get(&raw) {
            Some(existing) => existing.clone(),
            None => {
                let values: [f32; 4] =
                    std::array::from_fn(|i| raw[i].parse::<f32>().unwrap_or(0.0));
                let bbox = crate::geometry::map_bbox_0to1000_clamped(values, width, height);
                let encoded = crate::imaging::crop(table_image, bbox)
                    .and_then(|crop| crate::imaging::to_base64_data_url(&crop).ok());
                match encoded {
                    Some(url) => {
                        refs.insert(raw, url.clone());
                        url
                    }
                    // A region that does not overlap the crop at all has
                    // no pixels to emit; drop the marker rather than
                    // emitting a broken `<img src="">`.
                    None => continue,
                }
            }
        };
        out.push_str(&format!(
            "<img src=\"{}\" alt=\"embedded table image\" />",
            escape_cell_chunk(&image_ref)
        ));
    }
    out.push_str(&content[last_end..]);
    out
}

/// `core_runner.py::detect_repeat_token` — the repeat-loop detector.
///
/// Upstream's own algorithm, kept parameter-for-parameter: for every
/// suffix length `seq_len` up to `window_size / 2`, the allowed repeat
/// count is `int(base_max_repeats * (1 + scaling_factor / seq_len))`, and
/// the output is degenerate if its tail is that suffix repeated one more
/// time than allowed. Short repeating units are therefore tolerated far
/// more often than long ones (17 repeats of a single character, but only
/// 5 of a 12-character phrase).
///
/// Deliberately **not** `robustness::is_degenerate` (this project's own
/// sliding-window/coverage heuristic): the two disagree on real outputs,
/// and this protocol's reference implementation is the one that decides
/// what counts as degenerate here. `robustness.rs` is untouched and still
/// used by mineru-vlm.
///
/// Operates on `char`s, not bytes — upstream slices a Python `str`.
pub fn detect_repeat_token(
    predicted: &str,
    base_max_repeats: u32,
    window_size: usize,
    cut_from_end: usize,
    scaling_factor: f64,
) -> bool {
    let mut chars: Vec<char> = predicted.chars().collect();
    if cut_from_end > 0 {
        // Python's `s[:-n]` yields "" when `n >= len(s)`.
        let keep = chars.len().saturating_sub(cut_from_end);
        chars.truncate(keep);
    }
    let n = chars.len();
    if n == 0 {
        return false;
    }

    let max_seq_len = (window_size / 2).min(n);
    for seq_len in 1..=max_seq_len {
        let max_repeats =
            (base_max_repeats as f64 * (1.0 + scaling_factor / seq_len as f64)) as u32;
        let needed = (max_repeats as usize + 1) * seq_len;
        if needed > n {
            continue;
        }
        let candidate = &chars[n - seq_len..];
        let mut all_match = true;
        for k in 2..=(max_repeats as usize + 1) {
            let start = n - k * seq_len;
            if &chars[start..start + seq_len] != candidate {
                all_match = false;
                break;
            }
        }
        if all_match {
            return true;
        }
    }
    false
}

/// `core_runner.py::_should_retry_repeat_output`: the plain check, plus a
/// second pass with the last 50 characters removed — a loop that has
/// already started but is followed by a short tail would otherwise be
/// missed, since the detector only inspects the suffix.
pub fn should_retry_repeat_output(raw: &str) -> bool {
    if detect_repeat_token(raw, 4, 500, 0, 3.0) {
        return true;
    }
    raw.chars().count() > 50 && detect_repeat_token(raw, 4, 500, 50, 3.0)
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

    /// Golden values captured by **running upstream's own
    /// `core_runner.py::otsl_to_html`** on each input, not by reasoning
    /// about the code. Several of these encode upstream quirks that are
    /// reproduced on purpose (see the individual comments).
    #[test]
    fn otsl_to_html_matches_upstream_on_non_control_markup() {
        for (input, expected) in [
            // The bug this rewrite fixes: `<br>` is cell *content*. The
            // old `<([a-z]+)>` tokenizer consumed it as a control tag and
            // shifted every later cell one column left.
            (
                "<fcel>a<br>b<fcel>c<nl>",
                "<table><tr><td>a<br>b</td><td>c</td></tr></table>",
            ),
            // An opening tag passes through; a *closing* tag does not
            // match `<[A-Za-z][^>]*>` and is escaped as text. Upstream
            // behaves exactly this way.
            (
                "<fcel>a<b>bold</b><fcel>c<nl>",
                "<table><tr><td>a<b>bold&lt;/b&gt;</td><td>c</td></tr></table>",
            ),
            (
                "<fcel>nested<table><tr><td>z</td></tr></table><nl>",
                "<table><tr><td>nested<table><tr><td>z&lt;/td&gt;&lt;/tr&gt;&lt;/table&gt;</td></tr></table>",
            ),
            // `<otsl>` wrapper, case-insensitive, stripped recursively.
            (
                "<OTSL><fcel>x<nl></OTSL>",
                "<table><tr><td>x</td></tr></table>",
            ),
            // Whitespace is preserved — upstream: "Do not normalize
            // whitespace."
            (
                "<fcel> spaced <fcel>y<nl>",
                "<table><tr><td> spaced </td><td>y</td></tr></table>",
            ),
            // An existing entity survives; a bare `&` is escaped.
            (
                "<fcel>a&amp;b<fcel>a&b<nl>",
                "<table><tr><td>a&amp;b</td><td>a&amp;b</td></tr></table>",
            ),
            // Quotes are escaped too, unlike `otsl::escape_html`.
            (
                "<fcel>quote\"and'<nl>",
                "<table><tr><td>quote&quot;and&#x27;</td></tr></table>",
            ),
            // Newlines inside a cell become `<br>`.
            (
                "<fcel>line1\nline2<nl>",
                "<table><tr><td>line1<br>line2</td></tr></table>",
            ),
            // `html2otsl`'s private escape for a literal control tag.
            (
                "<fcel>\u{e100}fcel>lit<nl>",
                "<table><tr><td><fcel>lit</td></tr></table>",
            ),
            // Tokenization is case-insensitive but the tag dispatch is
            // not, so an uppercase tag advances the column and emits
            // nothing. Faithful to upstream, quirk included.
            ("<FCEL>up<NL>", "<table><tr></tr></table>"),
        ] {
            assert_eq!(otsl_to_html(input), expected, "input: {input:?}");
        }
    }

    /// Golden values captured by running upstream's own
    /// `detect_repeat_token` / `_should_retry_repeat_output`.
    #[test]
    fn repeat_detection_matches_upstream_python_output() {
        for (input, detect, should_retry) in [
            (String::new(), false, false),
            ("ab".to_string(), false, false),
            ("ab".repeat(50), true, true),
            // 17 repeats are needed for a single character (max_repeats
            // = int(4 * (1 + 3/1)) = 16), so 10 is fine and 20 is not.
            ("a".repeat(10), false, false),
            ("a".repeat(20), true, true),
            (
                "The quick brown fox jumps over the lazy dog, and then something else entirely happens."
                    .to_string(),
                false,
                false,
            ),
            // A 12-char phrase only tolerates 5 repeats.
            ("the cat sat ".repeat(4), false, false),
            ("the cat sat ".repeat(5), false, false),
            ("the cat sat ".repeat(10), true, true),
            // A loop followed by a long tail is caught by the suffix
            // check only after cutting 50 characters off the end...
            ("the cat sat ".repeat(10) + &"X".repeat(60), true, true),
            // ...and with a 10-char tail the plain check misses it while
            // the cut-50 pass still catches it.
            ("the cat sat ".repeat(10) + &"X".repeat(10), false, true),
        ] {
            assert_eq!(
                detect_repeat_token(&input, 4, 500, 0, 3.0),
                detect,
                "detect for {:?}",
                &input[..input.len().min(40)]
            );
            assert_eq!(
                should_retry_repeat_output(&input),
                should_retry,
                "should_retry for {:?}",
                &input[..input.len().min(40)]
            );
        }
    }

    #[test]
    fn repeat_detection_handles_multibyte_text_without_panicking() {
        // Byte-slicing would panic here; upstream slices characters.
        assert!(should_retry_repeat_output(&"重复".repeat(60)));
        assert!(!should_retry_repeat_output(
            "这是一句普通的中文句子，没有循环重复的内容。"
        ));
    }

    #[test]
    fn table_image_markers_are_cropped_and_inlined_as_base64() {
        // Two-tone, so the left and right halves genuinely encode
        // differently — a solid fill would make every crop byte-identical
        // and the dedup assertion below would pass for the wrong reason.
        let mut table = image::RgbImage::from_pixel(200, 100, image::Rgb([10, 20, 30]));
        for y in 0..100 {
            for x in 100..200 {
                table.put_pixel(x, y, image::Rgb([240, 200, 160]));
            }
        }
        // Two references to the same region plus one different region.
        let content =
            "a[img][0,0,500,1000][/img]b[img][0,0,500,1000][/img]c[img][500,0,1000,1000][/img]";
        let out = replace_table_image_markers(content, &table);

        assert_eq!(out.matches("<img src=\"data:image/png;base64,").count(), 3);
        assert_eq!(out.matches("alt=\"embedded table image\"").count(), 3);
        assert!(!out.contains("[img]"), "no marker may survive: {out}");
        assert!(out.starts_with('a') && out.contains('b') && out.ends_with("/>"));

        // Identical bboxes reuse one encoding (upstream's `refs` cache),
        // and a different bbox genuinely differs.
        let srcs: Vec<&str> = out.split("<img src=\"").skip(1).collect();
        assert_eq!(srcs.len(), 3);
        let first = srcs[0].split('"').next().unwrap();
        let second = srcs[1].split('"').next().unwrap();
        let third = srcs[2].split('"').next().unwrap();
        assert_eq!(first, second);
        assert_ne!(first, third);
    }

    #[test]
    fn content_without_table_image_markers_is_returned_untouched() {
        let table = image::RgbImage::from_pixel(10, 10, image::Rgb([0, 0, 0]));
        let content = "<table><tr><td>plain</td></tr></table>";
        assert_eq!(replace_table_image_markers(content, &table), content);
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

        #[test]
        fn repeat_detection_never_panics(s in ".*") {
            let _ = should_retry_repeat_output(&s);
        }
    }
}
