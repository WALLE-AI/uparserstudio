//! OTSL (Open Table Structure Language) token sequence → HTML, per
//! ARCHITECTURE.md §9.2 / T-1.5. Shared by mineru-vlm and (later)
//! monkeyocr. Token semantics: `fcel`=filled cell (text follows until the
//! next tag), `ecel`=empty cell, `lcel`=extend the cell to the left
//! (colspan), `ucel`=extend the cell above (rowspan), `xcel`=extend both,
//! `nl`=row break. Real model output can also emit literal `<table>`
//! HTML directly instead of OTSL tokens — that case short-circuits to a
//! passthrough.
//!
//! This is a best-effort general implementation of the published OTSL
//! grid semantics, not a verified port of MinerU's exact converter (that
//! source isn't available locally — see the P1 plan's caveat); malformed
//!/truncated token streams degrade to a best-effort partial table rather
//! than panicking.

use regex::Regex;
use std::collections::HashMap;
use std::sync::LazyLock;

static TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<(nl|fcel|ecel|lcel|ucel|xcel)>").expect("valid regex"));

struct Cell {
    text: String,
    row: usize,
    col: usize,
    max_row: usize,
    max_col: usize,
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

    let mut cells: Vec<Cell> = Vec::new();
    let mut grid: HashMap<(usize, usize), usize> = HashMap::new();
    let mut row = 0usize;
    let mut col = 0usize;
    let mut max_row_seen = 0usize;
    let mut max_col_seen = 0usize;

    let matches: Vec<_> = TAG_RE.captures_iter(trimmed).collect();
    if matches.is_empty() {
        return ("<table></table>".to_string(), warnings);
    }
    for (i, caps) in matches.iter().enumerate() {
        let tag = &caps[1];
        let tag_match = caps.get(0).unwrap();
        let text_start = tag_match.end();
        let text_end = matches
            .get(i + 1)
            .map(|next| next.get(0).unwrap().start())
            .unwrap_or(trimmed.len());
        let trailing = trimmed[text_start..text_end].trim();

        match tag {
            "nl" => {
                row += 1;
                col = 0;
            }
            "fcel" | "ecel" => {
                let idx = cells.len();
                cells.push(Cell {
                    text: if tag == "fcel" {
                        trailing.to_string()
                    } else {
                        String::new()
                    },
                    row,
                    col,
                    max_row: row,
                    max_col: col,
                });
                grid.insert((row, col), idx);
                max_row_seen = max_row_seen.max(row);
                max_col_seen = max_col_seen.max(col);
                col += 1;
            }
            "lcel" => {
                let referent = grid.get(&(row, col.wrapping_sub(1))).copied();
                let idx = referent.unwrap_or_else(|| new_fallback_cell(&mut cells, row, col));
                cells[idx].max_col = cells[idx].max_col.max(col);
                grid.insert((row, col), idx);
                max_row_seen = max_row_seen.max(row);
                max_col_seen = max_col_seen.max(col);
                col += 1;
            }
            "ucel" => {
                let referent = if row == 0 {
                    None
                } else {
                    grid.get(&(row - 1, col)).copied()
                };
                let idx = referent.unwrap_or_else(|| new_fallback_cell(&mut cells, row, col));
                cells[idx].max_row = cells[idx].max_row.max(row);
                grid.insert((row, col), idx);
                max_row_seen = max_row_seen.max(row);
                max_col_seen = max_col_seen.max(col);
                col += 1;
            }
            "xcel" => {
                // `xcel` extends both up and left, so it can have a
                // referent on either side. When both exist and disagree
                // (a genuinely ambiguous span), previously this silently
                // picked "up" with no consistency check or warning at
                // all — the exact spot this module's own doc comment
                // calls out as most likely to break on real irregular
                // table headers (see D.6 in `CLI_ENHANCEMENT_PROPOSAL.md`).
                // Now: prefer whichever neighbor already covers the
                // larger span (a reasonable tie-break — the bigger
                // existing merge is more likely the "real" origin cell),
                // and warn on the conflict rather than deciding silently.
                let referent_up = if row == 0 {
                    None
                } else {
                    grid.get(&(row - 1, col)).copied()
                };
                let referent_left = grid.get(&(row, col.wrapping_sub(1))).copied();
                let idx = match (referent_up, referent_left) {
                    (Some(up), Some(left)) if up != left => {
                        let span = |i: usize| {
                            (cells[i].max_row - cells[i].row + 1)
                                * (cells[i].max_col - cells[i].col + 1)
                        };
                        let chosen = if span(left) > span(up) { left } else { up };
                        warnings.push(format!(
                            "xcel span conflict at row {row} col {col}: up-neighbor and left-neighbor cells disagree, picked the {} span",
                            if chosen == left { "left" } else { "up" }
                        ));
                        chosen
                    }
                    (Some(up), _) => up,
                    (None, Some(left)) => left,
                    (None, None) => new_fallback_cell(&mut cells, row, col),
                };
                cells[idx].max_row = cells[idx].max_row.max(row);
                cells[idx].max_col = cells[idx].max_col.max(col);
                grid.insert((row, col), idx);
                max_row_seen = max_row_seen.max(row);
                max_col_seen = max_col_seen.max(col);
                col += 1;
            }
            _ => unreachable!("regex only matches known tags"),
        }
    }

    let html = render_html(&cells, &grid, max_row_seen, max_col_seen);
    (html, warnings)
}

fn new_fallback_cell(cells: &mut Vec<Cell>, row: usize, col: usize) -> usize {
    let idx = cells.len();
    cells.push(Cell {
        text: String::new(),
        row,
        col,
        max_row: row,
        max_col: col,
    });
    idx
}

fn render_html(
    cells: &[Cell],
    grid: &HashMap<(usize, usize), usize>,
    max_row: usize,
    max_col: usize,
) -> String {
    let mut html = String::from("<table>");
    for r in 0..=max_row {
        html.push_str("<tr>");
        for c in 0..=max_col {
            let Some(&idx) = grid.get(&(r, c)) else {
                continue;
            };
            let cell = &cells[idx];
            if cell.row != r || cell.col != c {
                continue; // not the origin cell of this span
            }
            let rowspan = cell.max_row - cell.row + 1;
            let colspan = cell.max_col - cell.col + 1;
            html.push_str("<td");
            if rowspan > 1 {
                html.push_str(&format!(" rowspan=\"{rowspan}\""));
            }
            if colspan > 1 {
                html.push_str(&format!(" colspan=\"{colspan}\""));
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
    fn xcel_span_conflict_between_distinct_up_and_left_cells_is_resolved_and_warned() {
        // Row 0: three independent single cells "a", "b", "c".
        // Row 1: "d", then an lcel (extends "d" rightward to col 1),
        // then an xcel at col 2 — its left-neighbor is the lcel-extended
        // "d" cell, and its up-neighbor is the independent "c" cell
        // (row0, col2). Those are two genuinely distinct cells, so this
        // is a real conflict rather than the same cell reached two ways.
        let raw = "<fcel>a<fcel>b<fcel>c<nl><fcel>d<lcel><xcel>";
        let (html, warnings) = to_html(raw);
        // Some resolution must have happened without panicking, and the
        // conflict must have been reported rather than silently decided.
        assert!(!warnings.is_empty(), "expected a span-conflict warning");
        assert!(warnings[0].contains("xcel span conflict"));
        assert!(html.starts_with("<table>"));
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
