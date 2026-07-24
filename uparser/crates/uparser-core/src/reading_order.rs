//! Geometric reading-order fallback (T-6.2), per ARCHITECTURE.md §4/§18
//! ("M3") — used by protocols whose `provides_reading_order()` is
//! `false` (currently `paddleocr` and `pipeline`; MonkeyOCRv2/mineru-vlm
//! carry their own model-native order and never call this). Implements a
//! recursive XY-cut (Nagy/Ha-style projection-profile splitting): find a
//! horizontal gap that splits the block set into a top group and a
//! bottom group with no block straddling the gap, recurse into each;
//! failing that, try a vertical gap (columns), recursing left-to-right;
//! failing both (a genuinely tangled overlapping set), fall back to
//! sorting by `(y0, x0)`. This is a from-scratch, best-effort general
//! implementation, not a port of `liteparse`'s `projection.rs` (that
//! module operates on liteparse's own per-character text items across
//! ~5000 lines of PDF-text-layer-specific heuristics — a different
//! problem shape from ordering already-detected `Block`-level bboxes,
//! and reading_order.rs must work with no `native`-feature dependency
//! since `paddleocr`/`pipeline` aren't feature-gated).

/// Assign `reading_order` (0-based) to `bboxes` via recursive XY-cut,
/// returning `bboxes[i]`'s order as `result[i]`. Deterministic and total
/// — every index gets a distinct order, even for degenerate/overlapping
/// input.
pub fn assign_reading_order(bboxes: &[[i32; 4]]) -> Vec<u32> {
    let indices: Vec<usize> = (0..bboxes.len()).collect();
    let ordered = xy_cut(indices, bboxes);

    let mut result = vec![0u32; bboxes.len()];
    for (order, idx) in ordered.into_iter().enumerate() {
        result[idx] = order as u32;
    }
    result
}

fn xy_cut(indices: Vec<usize>, boxes: &[[i32; 4]]) -> Vec<usize> {
    if indices.len() <= 1 {
        return indices;
    }

    // Columns before rows: a multi-column document (the common case this
    // fallback exists for) reads left-column-fully, then right-column,
    // not row-by-row across columns — so a clean vertical (X) gap takes
    // priority over a horizontal (Y) one when both would apply.
    if let Some(bands) = split_by_gap(&indices, boxes, Axis::X) {
        return bands
            .into_iter()
            .flat_map(|band| xy_cut(band, boxes))
            .collect();
    }
    if let Some(bands) = split_by_gap(&indices, boxes, Axis::Y) {
        return bands
            .into_iter()
            .flat_map(|band| xy_cut(band, boxes))
            .collect();
    }

    // Tangled set with no clean axis-aligned split: fall back to a
    // stable top-to-bottom, left-to-right sort rather than looping
    // forever trying to find a cut that doesn't exist.
    let mut sorted = indices;
    sorted.sort_by_key(|&i| (boxes[i][1], boxes[i][0]));
    sorted
}

#[derive(Clone, Copy)]
enum Axis {
    /// Split top group vs. bottom group (reading order: top first).
    Y,
    /// Split left column vs. right columns (reading order: left first).
    X,
}

/// Merge `indices`' intervals along `axis` and, if that yields more than
/// one disjoint band, return the indices partitioned into per-band
/// groups in band order. Returns `None` if everything merges into one
/// band (no clean cut along this axis).
fn split_by_gap(indices: &[usize], boxes: &[[i32; 4]], axis: Axis) -> Option<Vec<Vec<usize>>> {
    let (lo, hi) = match axis {
        Axis::Y => (1, 3),
        Axis::X => (0, 2),
    };

    let mut intervals: Vec<(i32, i32, usize)> = indices
        .iter()
        .map(|&i| (boxes[i][lo], boxes[i][hi], i))
        .collect();
    intervals.sort_by_key(|&(start, _, _)| start);

    let mut bands: Vec<(i32, i32, Vec<usize>)> = Vec::new();
    for (start, end, idx) in intervals.drain(..) {
        match bands.last_mut() {
            Some((_, band_end, members)) if start < *band_end => {
                *band_end = (*band_end).max(end);
                members.push(idx);
            }
            _ => bands.push((start, end, vec![idx])),
        }
    }

    if bands.len() <= 1 {
        return None;
    }
    Some(bands.into_iter().map(|(_, _, members)| members).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_column_top_to_bottom() {
        let boxes = [[0, 200, 100, 250], [0, 0, 100, 50], [0, 100, 100, 150]];
        let order = assign_reading_order(&boxes);
        // box 1 (y 0-50) first, box 2 (y 100-150) second, box 0 (y 200-250) third
        assert_eq!(order, vec![2, 0, 1]);
    }

    #[test]
    fn two_column_layout_reads_left_column_then_right_column() {
        // Two columns side by side, each with 2 stacked blocks. Correct
        // reading order: left-top, left-bottom, right-top, right-bottom
        // — not row-by-row (which would interleave columns).
        let boxes = [
            [0, 0, 90, 40],     // left col, top    -> want order 0
            [110, 0, 200, 40],  // right col, top    -> want order 2
            [0, 50, 90, 90],    // left col, bottom  -> want order 1
            [110, 50, 200, 90], // right col, bottom -> want order 3
        ];
        let order = assign_reading_order(&boxes);
        assert_eq!(order, vec![0, 2, 1, 3]);
    }

    #[test]
    fn empty_input_returns_empty() {
        let boxes: [[i32; 4]; 0] = [];
        assert_eq!(assign_reading_order(&boxes), Vec::<u32>::new());
    }

    #[test]
    fn single_block_gets_order_zero() {
        let boxes = [[0, 0, 10, 10]];
        assert_eq!(assign_reading_order(&boxes), vec![0]);
    }

    #[test]
    fn fully_overlapping_boxes_do_not_panic_and_produce_distinct_orders() {
        let boxes = [[0, 0, 100, 100], [0, 0, 100, 100], [0, 0, 100, 100]];
        let order = assign_reading_order(&boxes);
        let mut sorted = order.clone();
        sorted.sort();
        assert_eq!(sorted, vec![0, 1, 2]);
    }

    #[test]
    fn three_row_stack_reads_top_to_bottom_regardless_of_horizontal_position() {
        let boxes = [[50, 0, 150, 40], [0, 50, 100, 90], [100, 100, 200, 140]];
        let order = assign_reading_order(&boxes);
        assert_eq!(order, vec![0, 1, 2]);
    }
}
