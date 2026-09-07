//! Structure decisions the Markdown pipeline makes, exported for callers
//! that need the structure rather than the rendered string.
//!
//! # Why this exists
//!
//! This engine is a complete PDF→Markdown product: it detects headings (font
//! histograms, isolation, numbering) and tables (struct tree, ruling rects,
//! line geometry, heuristics), then formats both into Markdown text. Until
//! this module, those decisions were *only* observable as `#` and `|` in the
//! output string — a consumer building a structured IR from
//! `positioned_items` had to classify everything as plain text and threw away
//! the very analysis this engine exists to perform.
//!
//! # Contract
//!
//! Everything here is keyed by **geometry in PDF coordinates** (origin at the
//! bottom-left, same as [`crate::types::TextItem`]), not by item index.
//! Geometry is the stable currency between this engine's line grouping and a
//! consumer's own: index alignment would silently rot the first time either
//! side changes how it splits items, whereas a bbox either overlaps or it
//! does not.
//!
//! # Fidelity
//!
//! Hints are recorded at the exact point the Markdown writer commits to a
//! decision (where it emits the `#`, where it formats the table) — not at the
//! classifier that proposes candidates. A later veto in the writer therefore
//! removes the hint too, so the hints and the Markdown can never disagree.

use crate::tables::{Table, TableKind};

/// A line the Markdown writer emitted as a heading.
#[derive(Debug, Clone, PartialEq)]
pub struct HeadingHint {
    /// 1-indexed page.
    pub page: u32,
    /// `[x0, y0, x1, y1]` in PDF points, origin bottom-left.
    pub bbox: [f32; 4],
    /// Heading depth, 1..=6 — the number of `#` that were emitted.
    pub level: u8,
    /// The heading's plain text, for diagnostics and matching fallback.
    pub text: String,
}

/// A table the Markdown writer emitted.
#[derive(Debug, Clone, PartialEq)]
pub struct TableHint {
    /// 1-indexed page.
    pub page: u32,
    /// `[x0, y0, x1, y1]` in PDF points, origin bottom-left, derived from the
    /// detected row/column boundaries.
    pub bbox: [f32; 4],
    pub rows: usize,
    pub columns: usize,
    /// Cell text indexed `[row][column]`, exactly as the Markdown formatter
    /// received it.
    pub cells: Vec<Vec<String>>,
    /// A table of contents renders differently from a data table; consumers
    /// that rebuild the table need the same distinction.
    pub kind: TableKind,
    /// Where the writer placed this table in the flow, in the same order
    /// space as [`LineHint::order`].
    ///
    /// A table's geometry does not determine its position in the document:
    /// the writer inserts it relative to the *lines* it precedes, and on a
    /// multi-column page that is nowhere near what sorting by `y` would
    /// produce. Without this, a consumer can only guess — and guessing by
    /// geometry moves every table on a two-column page to the end of it.
    ///
    /// `None` when the table was detected but never emitted into the
    /// Markdown, which is also the signal that a consumer should not place
    /// it either.
    pub order: Option<usize>,
}

/// One text line as the Markdown writer grouped it.
///
/// The engine's grouping is column-aware and merge-aware (drop caps, wrapped
/// headings); a consumer clustering the same items by vertical proximity
/// alone fuses the left and right columns of a two-column page into single
/// lines. Exporting the grouping removes that whole class of error — and the
/// consumer's own clustering with it.
#[derive(Debug, Clone, PartialEq)]
pub struct LineHint {
    /// 1-indexed page.
    pub page: u32,
    /// `[x0, y0, x1, y1]` in PDF points, origin bottom-left.
    pub bbox: [f32; 4],
    /// Position in the document's reading order, 0-indexed.
    pub order: usize,
    /// The line's plain text, used to reconcile decisions the pipeline makes
    /// after geometry is gone (see [`reconcile_headings`]).
    pub text: String,
    /// `text` split into each item's own contribution, in item order and
    /// including the separator the writer chose before it, so
    /// `item_texts.concat() == text`.
    ///
    /// A consumer that re-joins the items itself gets the spacing wrong in
    /// ways that are invisible on prose and glaring on a table of contents,
    /// where a dot leader is dozens of separate items: the writer's rules
    /// produce `.................`, a gap threshold produces
    /// `. . . . . . . . .`.
    pub item_texts: Vec<String>,
}

/// Everything [`crate::process_pdf_mem`] learned about document structure
/// while producing Markdown.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StructureHints {
    pub headings: Vec<HeadingHint>,
    pub tables: Vec<TableHint>,
    pub lines: Vec<LineHint>,
}

impl StructureHints {
    pub fn is_empty(&self) -> bool {
        self.headings.is_empty() && self.tables.is_empty() && self.lines.is_empty()
    }

    /// The line containing `point` (an item's center), if any.
    ///
    /// Matching by a point rather than by box overlap because the consumer
    /// assigns *items* to lines, and an item is either inside a line's box or
    /// belongs to a different line.
    pub fn line_at(&self, page: u32, point: [f32; 2]) -> Option<&LineHint> {
        self.lines
            .iter()
            .filter(|hint| hint.page == page)
            .find(|hint| {
                point[0] >= hint.bbox[0] - LINE_MATCH_TOLERANCE
                    && point[0] <= hint.bbox[2] + LINE_MATCH_TOLERANCE
                    && point[1] >= hint.bbox[1] - LINE_MATCH_TOLERANCE
                    && point[1] <= hint.bbox[3] + LINE_MATCH_TOLERANCE
            })
    }

    /// The heading covering `bbox`, if any.
    ///
    /// A consumer's line grouping rarely produces byte-identical boxes, so
    /// this asks whether the query is *mostly inside* a hint rather than
    /// equal to it.
    pub fn heading_at(&self, page: u32, bbox: [f32; 4]) -> Option<&HeadingHint> {
        self.headings
            .iter()
            .filter(|hint| hint.page == page)
            .find(|hint| overlap_ratio(bbox, hint.bbox) >= CONTAINMENT_RATIO)
    }

    /// The table that claims a line with this box and text, if any.
    ///
    /// Two accept paths, because a table's bounds come from its detected row
    /// and column *boundaries*: a one-column table has a single column
    /// boundary and therefore a zero-width box, which no line can overlap.
    /// Such a table is matched by vertical span plus cell-text identity
    /// instead — a line inside the table's rows whose text is one of its
    /// cells belongs to it.
    pub fn table_at(&self, page: u32, bbox: [f32; 4], text: &str) -> Option<&TableHint> {
        let text = text.trim();
        self.tables
            .iter()
            .filter(|hint| hint.page == page)
            .find(|hint| {
                if overlap_ratio(bbox, hint.bbox) >= CONTAINMENT_RATIO {
                    return true;
                }
                if text.is_empty() {
                    return false;
                }
                let center_y = (bbox[1] + bbox[3]) / 2.0;
                let within_rows = center_y >= hint.bbox[1] - LINE_MATCH_TOLERANCE
                    && center_y <= hint.bbox[3] + LINE_MATCH_TOLERANCE;
                if !within_rows {
                    return false;
                }
                let line = compact(text);
                if line.is_empty() {
                    return false;
                }
                // One cell (a one-column table), or a whole row concatenated (the
                // consumer sees a table row as a single line of text, since the
                // column gap is not a line break).
                hint.cells
                    .iter()
                    .flatten()
                    .any(|cell| compact(cell) == line)
                    || hint.cells.iter().any(|row| {
                        !row.is_empty()
                            && row.iter().map(|cell| compact(cell)).collect::<String>() == line
                    })
            })
    }
}

/// How much of a query box must fall inside a hint box to count as belonging
/// to it. Not 1.0: a consumer's line box can extend a point or two past the
/// hint's because the two sides derive bounds from different item subsets
/// (a table hint's bounds come from row/column rulings, not from glyphs).
const CONTAINMENT_RATIO: f32 = 0.7;

/// Slack when testing whether an item's center sits on a line. The line box is
/// the union of its items' boxes, so a center is normally strictly inside;
/// the tolerance only absorbs float rounding.
const LINE_MATCH_TOLERANCE: f32 = 0.5;

/// Text with all whitespace removed, so a comparison ignores where the
/// consumer put spaces between columns.
fn compact(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Rebuild the heading hints from the Markdown the pipeline finally emitted.
///
/// The writer is not the only place headings are decided: the Markdown
/// post-pass promotes a bold uppercase line to `#`, demotes runs of visual
/// headings and drops duplicates — all after geometry is gone, working on the
/// string. Hints recorded at the writer therefore both **miss** promotions and
/// **keep** demoted headings.
///
/// Reconciling against the final output makes the export describe the document
/// that was actually produced, whatever combination of passes produced it.
/// Matching is by text because that is the only identity the post-pass has;
/// geometry comes from the line the text belongs to.
pub(crate) fn reconcile_headings(
    markdown: &str,
    lines: &[LineHint],
    writer_hints: Vec<HeadingHint>,
) -> Vec<HeadingHint> {
    // Occurrences, not a single entry per text: a document that says
    // "Version History" twice (a page title and the section below it) emits
    // two headings, and collapsing them onto one writer hint gives both the
    // *first* one's geometry — so the second heading has no line of its own
    // and a consumer matching by geometry never sees it.
    let mut by_text: std::collections::HashMap<String, std::collections::VecDeque<&HeadingHint>> =
        std::collections::HashMap::new();
    for hint in &writer_hints {
        by_text
            .entry(compact(&hint.text))
            .or_default()
            .push_back(hint);
    }
    let mut used_lines: std::collections::HashSet<usize> = std::collections::HashSet::new();

    let mut out = Vec::new();
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        let level = trimmed.chars().take_while(|c| *c == '#').count();
        if level == 0 || level > 6 {
            continue;
        }
        let Some(text) = trimmed[level..].strip_prefix(' ') else {
            continue;
        };
        let key = compact(&strip_emphasis(text));
        if key.is_empty() {
            continue;
        }
        if let Some(hint) = by_text.get_mut(&key).and_then(|queue| queue.pop_front()) {
            // Claim the line this hint came from, so a later heading with the
            // same text falls through to a *different* line rather than
            // recovering the geometry this one already used.
            if let Some(index) = lines
                .iter()
                .position(|line| line.page == hint.page && line.bbox == hint.bbox)
            {
                used_lines.insert(index);
            }
            out.push(HeadingHint {
                level: level.clamp(1, 6) as u8,
                ..hint.clone()
            });
            continue;
        }
        // Promoted after the writer ran: recover geometry from the line whose
        // text this heading came from.
        if let Some((index, source)) = lines.iter().enumerate().find(|(index, candidate)| {
            !used_lines.contains(index) && compact(&strip_emphasis(&candidate.text)) == key
        }) {
            used_lines.insert(index);
            out.push(HeadingHint {
                page: source.page,
                bbox: source.bbox,
                level: level.clamp(1, 6) as u8,
                text: text.to_owned(),
            });
        }
    }
    out
}

/// Drop the inline markup the writer may have wrapped a line in, so
/// `**IMPLEMENTATION**` and `<u>Reference frameworks:</u>` compare equal to
/// the raw line text they came from.
fn strip_emphasis(text: &str) -> String {
    let without_emphasis: String = text
        .chars()
        .filter(|character| !matches!(character, '*' | '_'))
        .collect();
    let mut out = String::with_capacity(without_emphasis.len());
    let mut depth = 0usize;
    for character in without_emphasis.chars() {
        match character {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(character),
            _ => {}
        }
    }
    out
}

fn overlap_ratio(query: [f32; 4], region: [f32; 4]) -> f32 {
    // An inverted box is malformed, not degenerate: clamping its extents to
    // zero would turn it into a point and let it match whatever contains that
    // point. A matching predicate should refuse input it cannot interpret.
    if query[2] < query[0] || query[3] < query[1] {
        return 0.0;
    }
    let width = query[2] - query[0];
    let height = query[3] - query[1];
    let area = width * height;
    if area <= f32::EPSILON {
        // A zero-area query (an empty line) is "inside" only if its origin is.
        return if query[0] >= region[0]
            && query[0] <= region[2]
            && query[1] >= region[1]
            && query[1] <= region[3]
        {
            1.0
        } else {
            0.0
        };
    }
    let overlap_width = (query[2].min(region[2]) - query[0].max(region[0])).max(0.0);
    let overlap_height = (query[3].min(region[3]) - query[1].max(region[1])).max(0.0);
    overlap_width * overlap_height / area
}

/// Bounding box of a set of text items, in PDF coordinates.
pub(crate) fn items_bbox(items: &[crate::types::TextItem]) -> Option<[f32; 4]> {
    let first = items.first()?;
    let mut bbox = [
        first.x,
        first.y,
        first.x + first.width,
        first.y + first.height,
    ];
    for item in &items[1..] {
        bbox[0] = bbox[0].min(item.x);
        bbox[1] = bbox[1].min(item.y);
        bbox[2] = bbox[2].max(item.x + item.width);
        bbox[3] = bbox[3].max(item.y + item.height);
    }
    Some(bbox)
}

/// Bounding box of a detected table, from its row and column boundaries.
pub(crate) fn table_bbox(table: &Table) -> [f32; 4] {
    let x_min = table.columns.iter().copied().fold(f32::INFINITY, f32::min);
    let x_max = table
        .columns
        .iter()
        .copied()
        .fold(f32::NEG_INFINITY, f32::max);
    let y_min = table.rows.iter().copied().fold(f32::INFINITY, f32::min);
    let y_max = table.rows.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    if x_min.is_finite() && x_max.is_finite() && y_min.is_finite() && y_max.is_finite() {
        [x_min, y_min, x_max, y_max]
    } else {
        [0.0, 0.0, 0.0, 0.0]
    }
}

/// Bounding box of the items a table actually claimed.
///
/// [`table_bbox`] derives the box from the detected row and column
/// *boundaries*, which is degenerate whenever a table has a single row or a
/// single column: one boundary means zero extent on that axis, and a
/// zero-extent box overlaps nothing. The items are the table, so their union
/// is both non-degenerate and a tighter description of where it sits.
///
/// `item_indices` are positions in whatever slice the detector was handed;
/// an index the caller cannot resolve means this table came from somewhere
/// the caller did not describe, and the boundary-derived box is all there is.
fn claimed_items_bbox(table: &Table, items: &[crate::types::TextItem]) -> Option<[f32; 4]> {
    let claimed: Vec<crate::types::TextItem> = table
        .item_indices
        .iter()
        .filter_map(|&index| items.get(index).cloned())
        .collect();
    items_bbox(&claimed)
}

pub(crate) fn table_hint(table: &Table, page: u32, items: &[crate::types::TextItem]) -> TableHint {
    // The rendered cells, not the raw detection output — see
    // `rendered_table_cells`.
    let cells = crate::tables::rendered_table_cells(table);
    TableHint {
        page,
        bbox: claimed_items_bbox(table, items).unwrap_or_else(|| table_bbox(table)),
        rows: cells.len(),
        columns: cells.first().map(Vec::len).unwrap_or(0),
        cells,
        kind: table.kind,
        // Filled in after the writer runs — see `assign_table_orders`.
        order: None,
    }
}

/// Attach each emitted table's flow position to its hint.
///
/// The writer identifies a table by `(page, index within that page)`; the
/// hints are a flat vector appended in the same order tables are pushed onto
/// their page's list, so the *n*-th hint on a page is that page's *n*-th
/// table. Keeping the correlation here, in one place, means the writer only
/// has to report what it actually emitted.
pub(crate) fn assign_table_orders(hints: &mut [TableHint], emitted: &[(u32, usize, usize)]) {
    let mut seen_per_page: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
    for hint in hints.iter_mut() {
        let index = seen_per_page.entry(hint.page).or_insert(0);
        let position = *index;
        *index += 1;
        hint.order = emitted
            .iter()
            .find(|(page, table_index, _)| *page == hint.page && *table_index == position)
            .map(|(_, _, order)| *order);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(page: u32, bbox: [f32; 4], order: usize) -> LineHint {
        LineHint {
            page,
            bbox,
            order,
            text: String::new(),
            item_texts: Vec::new(),
        }
    }

    fn heading(page: u32, bbox: [f32; 4], level: u8) -> HeadingHint {
        HeadingHint {
            page,
            bbox,
            level,
            text: "x".to_owned(),
        }
    }

    #[test]
    fn containment_tolerates_a_slightly_larger_query_box() {
        let hints = StructureHints {
            headings: vec![heading(1, [100.0, 700.0, 300.0, 720.0], 2)],
            ..Default::default()
        };
        // Exactly the hint.
        assert_eq!(
            hints
                .heading_at(1, [100.0, 700.0, 300.0, 720.0])
                .unwrap()
                .level,
            2
        );
        // A line box a couple of points wider still matches.
        assert!(hints.heading_at(1, [98.0, 699.0, 302.0, 721.0]).is_some());
        // A different line on the same page does not.
        assert!(hints.heading_at(1, [100.0, 600.0, 300.0, 620.0]).is_none());
        // Right geometry, wrong page.
        assert!(hints.heading_at(2, [100.0, 700.0, 300.0, 720.0]).is_none());
    }

    #[test]
    fn a_line_half_outside_the_region_does_not_match() {
        let hints = StructureHints {
            headings: vec![heading(1, [0.0, 0.0, 100.0, 10.0], 1)],
            ..Default::default()
        };
        // 50% inside — below the 0.7 threshold.
        assert!(hints.heading_at(1, [50.0, 0.0, 150.0, 10.0]).is_none());
        // 80% inside — above it.
        assert!(hints.heading_at(1, [20.0, 0.0, 120.0, 10.0]).is_some());
    }

    /// The case this exists for: two columns at the same height must stay
    /// two lines, which a y-proximity clustering cannot express.
    #[test]
    fn two_columns_at_the_same_height_are_distinct_lines() {
        let hints = StructureHints {
            lines: vec![
                line(1, [50.0, 700.0, 280.0, 712.0], 0),
                line(1, [320.0, 700.0, 550.0, 712.0], 1),
            ],
            ..Default::default()
        };
        assert_eq!(hints.line_at(1, [100.0, 706.0]).unwrap().order, 0);
        assert_eq!(hints.line_at(1, [400.0, 706.0]).unwrap().order, 1);
        // The gutter belongs to neither.
        assert!(hints.line_at(1, [300.0, 706.0]).is_none());
    }

    /// A one-column table has a single column boundary, so its box is
    /// zero-width and overlap matching cannot work; the vertical span plus
    /// cell text identifies its lines instead.
    #[test]
    fn a_zero_width_table_still_claims_its_own_cell_lines() {
        let hints = StructureHints {
            tables: vec![TableHint {
                page: 1,
                bbox: [100.0, 500.0, 100.0, 620.0],
                rows: 2,
                columns: 1,
                cells: vec![vec!["Framework".to_owned()], vec!["#1: Recycle".to_owned()]],
                kind: TableKind::Data,
                order: None,
            }],
            ..Default::default()
        };
        assert!(hints
            .table_at(1, [100.0, 600.0, 300.0, 612.0], "Framework")
            .is_some());
        assert!(hints
            .table_at(1, [100.0, 540.0, 300.0, 552.0], "#1: Recycle")
            .is_some());
        // Same vertical band, text that is not a cell of this table.
        assert!(hints
            .table_at(1, [100.0, 600.0, 300.0, 612.0], "Unrelated line")
            .is_none());
        // Right text, outside the table's rows.
        assert!(hints
            .table_at(1, [100.0, 100.0, 300.0, 112.0], "Framework")
            .is_none());
    }

    /// A multi-column table's box stops at the last column's *left* edge, so
    /// a line spanning the full width is only half inside it. The line is one
    /// whole row of the table, which is what identifies it.
    #[test]
    fn a_row_spanning_line_is_claimed_by_its_table() {
        let hints = StructureHints {
            tables: vec![TableHint {
                page: 1,
                bbox: [72.0, 500.0, 200.0, 620.0],
                rows: 2,
                columns: 2,
                cells: vec![
                    vec![
                        "Potosi Pupfish".to_owned(),
                        "Cyprinodon alvarezi".to_owned(),
                    ],
                    vec!["Golden Skiffia".to_owned(), "Skiffia francesae".to_owned()],
                ],
                kind: TableKind::Data,
                order: None,
            }],
            ..Default::default()
        };
        assert!(hints
            .table_at(
                1,
                [72.0, 600.0, 500.0, 612.0],
                "Potosi Pupfish Cyprinodon alvarezi"
            )
            .is_some());
        assert!(
            hints
                .table_at(
                    1,
                    [72.0, 540.0, 500.0, 552.0],
                    "Golden SkiffiaSkiffia francesae"
                )
                .is_some(),
            "column gaps are not always spaces"
        );
        // A line in the same band that is not a row of this table.
        assert!(hints
            .table_at(1, [72.0, 600.0, 500.0, 612.0], "Some caption text")
            .is_none());
    }

    /// A page title and a section below it can carry the same words. Both
    /// are emitted, so both need their own geometry — giving the second one
    /// the first one's box hides it from a consumer matching by geometry,
    /// which is how a `##` in the Markdown ends up as plain text in the IR.
    #[test]
    fn a_heading_repeated_verbatim_resolves_to_its_own_line() {
        let title = LineHint {
            page: 1,
            bbox: [70.0, 714.0, 225.0, 735.0],
            order: 0,
            text: "Version History".to_owned(),
            item_texts: vec!["Version History".to_owned()],
        };
        let section = LineHint {
            page: 1,
            bbox: [70.0, 443.0, 186.0, 458.0],
            order: 7,
            text: "Version History".to_owned(),
            item_texts: vec!["Version History".to_owned()],
        };
        let writer = vec![HeadingHint {
            page: 1,
            bbox: title.bbox,
            level: 1,
            text: "Version History".to_owned(),
        }];

        let out = reconcile_headings(
            "# Version History\n\nbody\n\n## Version History\n",
            &[title.clone(), section.clone()],
            writer,
        );

        assert_eq!(out.len(), 2);
        assert_eq!((out[0].level, out[0].bbox), (1, title.bbox));
        assert_eq!((out[1].level, out[1].bbox), (2, section.bbox));
    }

    /// The writer names a table by `(page, index within that page)`; the
    /// hints are one flat vector. Getting the correlation wrong hands a
    /// table another table's position, which reads as a reordering bug far
    /// from its cause.
    #[test]
    fn table_orders_are_matched_per_page_not_globally() {
        fn hint(page: u32) -> TableHint {
            TableHint {
                page,
                bbox: [0.0, 0.0, 10.0, 10.0],
                rows: 1,
                columns: 1,
                cells: vec![vec!["x".to_owned()]],
                kind: TableKind::Data,
                order: None,
            }
        }
        // Page 1 has two tables, page 2 has one; the second table on page 2
        // was detected but never emitted.
        let mut hints = vec![hint(1), hint(1), hint(2), hint(2)];
        assign_table_orders(&mut hints, &[(1, 0, 3), (1, 1, 11), (2, 0, 20)]);

        assert_eq!(hints[0].order, Some(3));
        assert_eq!(hints[1].order, Some(11));
        assert_eq!(hints[2].order, Some(20));
        assert_eq!(
            hints[3].order, None,
            "a table the writer never emitted has no position"
        );
    }

    #[test]
    fn empty_hints_never_match() {
        let hints = StructureHints::default();
        assert!(hints.is_empty());
        assert!(hints.heading_at(1, [0.0, 0.0, 10.0, 10.0]).is_none());
        assert!(hints.table_at(1, [0.0, 0.0, 10.0, 10.0], "x").is_none());
        assert!(hints.line_at(1, [5.0, 5.0]).is_none());
    }

    #[test]
    fn degenerate_boxes_do_not_panic_or_divide_by_zero() {
        let hints = StructureHints {
            headings: vec![heading(1, [0.0, 0.0, 100.0, 10.0], 1)],
            ..Default::default()
        };
        assert!(hints.heading_at(1, [50.0, 5.0, 50.0, 5.0]).is_some());
        assert!(hints.heading_at(1, [500.0, 5.0, 500.0, 5.0]).is_none());
        // An inverted box is refused outright, even though its origin sits
        // inside the region.
        assert!(hints.heading_at(1, [90.0, 9.0, 10.0, 1.0]).is_none());
    }
}
