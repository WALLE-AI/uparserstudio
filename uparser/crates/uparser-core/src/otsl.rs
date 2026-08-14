//! OTSL (Open Table Structure Language) token sequence → HTML, per
//! ARCHITECTURE.md §9.2 / T-1.5. Shared by mineru-vlm and (later)
//! monkeyocr. Token semantics: `fcel`=filled cell (text follows until the
//! next tag), `ecel`=empty cell, `lcel`=extend the cell to the left
//! (colspan), `ucel`=extend the cell above (rowspan), `xcel`=extend both,
//! `nl`=row break. Real model output can also emit literal `<table>`
//! HTML directly instead of OTSL tokens — that case short-circuits to a
//! passthrough.
//!
//! The converter follows `mineru_vl_utils` v1.0.5's `otsl2html.py` shape:
//! split into row token grids, pad ragged rows with `<ecel>`, then compute
//! spans for `<fcel>/<ecel>` origin cells by scanning right/down extension
//! tokens. Malformed/truncated streams degrade to a best-effort partial
//! table rather than panicking.

use regex::Regex;
use std::sync::LazyLock;

static TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<(nl|fcel|ecel|lcel|ucel|xcel)>").expect("valid regex"));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Token {
    Nl,
    Fcel,
    Ecel,
    Lcel,
    Ucel,
    Xcel,
}

struct Cell {
    text: String,
    row: usize,
    col: usize,
    row_span: usize,
    col_span: usize,
}

/// Convert OTSL tokens (or a passthrough literal `<table>...</table>`) to
/// an HTML `<table>` string. Returns any non-fatal recovery/conflict
/// warnings alongside the HTML — see D.6/D.7 in
/// `CLI_ENHANCEMENT_PROPOSAL.md`.
pub fn to_html(raw: &str) -> (String, Vec<String>) {
    let mut warnings = Vec::new();
    let trimmed = raw.trim();
    let lower = trimmed.to_lowercase();
    if lower.starts_with("<table") {
        if lower.ends_with("</table>") {
            return (trimmed.to_string(), warnings);
        }
        // Starts with `<table` but doesn't actually close — previously
        // this was trusted as complete HTML and passed through verbatim
        // regardless (D.7). Fall through to OTSL tokenization instead;
        // if that also fails to find any tags below, at least warn
        // rather than silently emitting truncated/unclosed HTML.
        warnings.push(
            "input starts with `<table` but doesn't end with `</table>` — not trusting it as complete HTML passthrough".to_string(),
        );
    }

    let items = tokenize(trimmed);
    if items.is_empty() {
        return ("<table></table>".to_string(), warnings);
    }

    let mut rows = split_rows(&items);
    if rows.is_empty() {
        return ("<table></table>".to_string(), warnings);
    }
    pad_rows(&mut rows, &mut warnings);

    let cells = parse_cells(&rows, &items);
    let html = render_html(&cells, rows.len(), rows.first().map_or(0, Vec::len));
    (html, warnings)
}

fn tokenize(input: &str) -> Vec<(Token, String)> {
    let matches: Vec<_> = TAG_RE.captures_iter(input).collect();
    matches
        .iter()
        .enumerate()
        .map(|(i, caps)| {
            let tag_match = caps.get(0).unwrap();
            let text_start = tag_match.end();
            let text_end = matches
                .get(i + 1)
                .map(|next| next.get(0).unwrap().start())
                .unwrap_or(input.len());
            (
                parse_tag(&caps[1]),
                input[text_start..text_end].trim().to_string(),
            )
        })
        .collect()
}

fn parse_tag(tag: &str) -> Token {
    match tag {
        "nl" => Token::Nl,
        "fcel" => Token::Fcel,
        "ecel" => Token::Ecel,
        "lcel" => Token::Lcel,
        "ucel" => Token::Ucel,
        "xcel" => Token::Xcel,
        _ => unreachable!("regex only matches known tags"),
    }
}

fn split_rows(items: &[(Token, String)]) -> Vec<Vec<Token>> {
    let mut rows = Vec::new();
    let mut current = Vec::new();
    for (token, _) in items {
        if *token == Token::Nl {
            if !current.is_empty() {
                rows.push(std::mem::take(&mut current));
            }
        } else {
            current.push(*token);
        }
    }
    if !current.is_empty() {
        rows.push(current);
    }
    rows
}

fn pad_rows(rows: &mut [Vec<Token>], warnings: &mut Vec<String>) {
    let Some(max_cols) = rows.iter().map(Vec::len).max() else {
        return;
    };
    for (row_idx, row) in rows.iter_mut().enumerate() {
        if row.len() < max_cols {
            warnings.push(format!(
                "padded ragged OTSL row {row_idx} from {} to {max_cols} columns with <ecel>",
                row.len()
            ));
            row.resize(max_cols, Token::Ecel);
        }
    }
}

fn parse_cells(rows: &[Vec<Token>], items: &[(Token, String)]) -> Vec<Cell> {
    let mut cells = Vec::new();
    let mut row = 0usize;
    let mut col = 0usize;
    for (token, text) in items {
        match token {
            Token::Nl => {
                row += 1;
                col = 0;
            }
            Token::Fcel | Token::Ecel => {
                if row < rows.len() && col < rows[row].len() {
                    cells.push(Cell {
                        text: if *token == Token::Fcel {
                            text.trim().to_string()
                        } else {
                            String::new()
                        },
                        row,
                        col,
                        row_span: 1 + count_down(rows, row + 1, col, &[Token::Ucel, Token::Xcel]),
                        col_span: 1 + count_right(rows, row, col + 1, &[Token::Lcel, Token::Xcel]),
                    });
                }
                col += 1;
            }
            Token::Lcel | Token::Ucel | Token::Xcel => {
                col += 1;
            }
        }
    }
    cells
}

fn count_right(rows: &[Vec<Token>], row: usize, mut col: usize, tokens: &[Token]) -> usize {
    let mut span = 0;
    while row < rows.len() && col < rows[row].len() && tokens.contains(&rows[row][col]) {
        span += 1;
        col += 1;
    }
    span
}

fn count_down(rows: &[Vec<Token>], mut row: usize, col: usize, tokens: &[Token]) -> usize {
    let mut span = 0;
    while row < rows.len() && col < rows[row].len() && tokens.contains(&rows[row][col]) {
        span += 1;
        row += 1;
    }
    span
}

fn render_html(cells: &[Cell], num_rows: usize, num_cols: usize) -> String {
    let mut html = String::from("<table>");
    for r in 0..num_rows {
        html.push_str("<tr>");
        for c in 0..num_cols {
            if covered_by_previous_span(cells, r, c) {
                continue;
            }
            let Some(cell) = cells.iter().find(|cell| cell.row == r && cell.col == c) else {
                continue;
            };
            html.push_str("<td");
            if cell.row_span > 1 {
                html.push_str(&format!(" rowspan=\"{}\"", cell.row_span));
            }
            if cell.col_span > 1 {
                html.push_str(&format!(" colspan=\"{}\"", cell.col_span));
            }
            html.push('>');
            html.push_str(&escape_html(&cell.text));
            html.push_str("</td>");
        }
        html.push_str("</tr>");
    }
    html.push_str("</table>");
    html
}

fn covered_by_previous_span(cells: &[Cell], row: usize, col: usize) -> bool {
    cells.iter().any(|cell| {
        (cell.row != row || cell.col != col)
            && row >= cell.row
            && row < cell.row + cell.row_span
            && col >= cell.col
            && col < cell.col + cell.col_span
    })
}

pub(crate) fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_literal_html() {
        let raw = "<table><tr><td>a</td></tr></table>";
        let (html, warnings) = to_html(raw);
        assert_eq!(html, raw);
        assert!(warnings.is_empty());
    }

    #[test]
    fn simple_2x2_grid() {
        let raw = "<fcel>a<fcel>b<nl><fcel>c<fcel>d";
        let (html, warnings) = to_html(raw);
        assert_eq!(
            html,
            "<table><tr><td>a</td><td>b</td></tr><tr><td>c</td><td>d</td></tr></table>"
        );
        assert!(warnings.is_empty());
    }

    #[test]
    fn empty_cell_renders_blank_td() {
        let raw = "<fcel>a<ecel><nl><fcel>c<fcel>d";
        let (html, _) = to_html(raw);
        assert!(html.contains("<td></td>"));
    }

    #[test]
    fn colspan_via_lcel() {
        // row: one filled cell spanning two columns
        let raw = "<fcel>a<lcel><nl><fcel>c<fcel>d";
        let (html, _) = to_html(raw);
        assert!(html.contains("colspan=\"2\""));
        assert!(!html.contains("rowspan"));
    }

    #[test]
    fn rowspan_via_ucel() {
        let raw = "<fcel>a<fcel>b<nl><ucel><fcel>d";
        let (html, _) = to_html(raw);
        assert!(html.contains("rowspan=\"2\""));
    }

    #[test]
    fn two_d_span_via_xcel() {
        // 2x2 block all merged into a single cell via lcel/ucel/xcel
        let raw = "<fcel>a<lcel><nl><ucel><xcel>";
        let (html, warnings) = to_html(raw);
        assert!(html.contains("rowspan=\"2\""));
        assert!(html.contains("colspan=\"2\""));
        // The up-neighbor and left-neighbor here are the same cell (the
        // 2x2 block has already been fully merged by the time xcel
        // runs), so this isn't a genuine conflict — no warning expected.
        assert!(warnings.is_empty());
    }

    #[test]
    fn xcel_with_distinct_neighbors_still_renders_stably() {
        // Row 0: three independent single cells "a", "b", "c".
        // Row 1: "d", then an lcel (extends "d" rightward to col 1),
        // then an xcel at col 2 — its left-neighbor is the lcel-extended
        // "d" cell, and its up-neighbor is the independent "c" cell
        // (row0, col2). Those are two genuinely distinct cells, so this
        // is a real conflict rather than the same cell reached two ways.
        let raw = "<fcel>a<fcel>b<fcel>c<nl><fcel>d<lcel><xcel>";
        let (html, warnings) = to_html(raw);
        // v1.0.5's two-pass converter derives spans from origin cells
        // only; xcel itself is not a conflict-resolution event.
        assert!(warnings.is_empty());
        assert!(html.starts_with("<table>"));
        assert!(html.contains("colspan=\"3\""));
    }

    #[test]
    fn cell_text_is_html_escaped() {
        let raw = "<fcel>1 < 2 & 3 > 0";
        let (html, _) = to_html(raw);
        assert!(html.contains("1 &lt; 2 &amp; 3 &gt; 0"));
    }

    #[test]
    fn truncated_stream_does_not_panic() {
        let raw = "<fcel>a<nl><ucel><xcel><lcel>";
        let _ = to_html(raw);
    }

    #[test]
    fn empty_input_yields_empty_table() {
        let (html, warnings) = to_html("");
        assert_eq!(html, "<table></table>");
        assert!(warnings.is_empty());
    }

    #[test]
    fn unclosed_table_like_prefix_is_not_trusted_as_passthrough() {
        // Starts with `<table` but never closes — previously this was
        // blindly trusted as complete HTML and passed through as-is
        // (D.7). It should fall through to OTSL tokenization (which
        // finds no OTSL tags here either) and warn, rather than
        // silently emitting truncated HTML.
        let raw = "<table><tr><td>a</td></tr>";
        let (html, warnings) = to_html(raw);
        assert_ne!(html, raw);
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("doesn't end with `</table>`"))
        );
    }

    #[test]
    fn properly_closed_table_passthrough_is_still_trusted() {
        let raw = "<TABLE><tr><td>a</td></tr></TABLE>";
        let (html, warnings) = to_html(raw);
        assert_eq!(html, raw);
        assert!(warnings.is_empty());
    }

    proptest::proptest! {
        #[test]
        fn arbitrary_input_never_panics(s in ".*") {
            let _ = to_html(&s);
        }
    }
}
