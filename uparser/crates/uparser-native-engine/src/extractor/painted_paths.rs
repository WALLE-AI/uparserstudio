//! General painted-path extraction and vector-figure region detection.
//!
//! This intentionally uses drawing operators and geometry only. It does not
//! depend on filenames, page numbers, captions, or document-specific labels.

use super::xobjects::{get_form_xobjects, get_page_xobjects, XObjectType};
use super::{get_number, multiply_matrices};
use crate::types::{ItemType, PdfPaintedPath, TextItem};
use lopdf::content::{Content, Operation};
use lopdf::{Document, Object, ObjectId};
use std::collections::{HashMap, HashSet};

const MAX_FORM_DEPTH: u8 = 5;

#[derive(Default)]
struct PathState {
    bbox: Option<[f32; 4]>,
    current: Option<(f32, f32)>,
    start: Option<(f32, f32)>,
    segment_count: u32,
    has_curve: bool,
    has_diagonal: bool,
}

impl PathState {
    fn point(&mut self, point: (f32, f32)) {
        if !point.0.is_finite() || !point.1.is_finite() {
            return;
        }
        self.bbox = Some(match self.bbox {
            Some([x0, y0, x1, y1]) => [
                x0.min(point.0),
                y0.min(point.1),
                x1.max(point.0),
                y1.max(point.1),
            ],
            None => [point.0, point.1, point.0, point.1],
        });
    }

    fn move_to(&mut self, point: (f32, f32)) {
        self.point(point);
        self.current = Some(point);
        self.start = Some(point);
    }

    fn line_to(&mut self, point: (f32, f32)) {
        if let Some(current) = self.current {
            let dx = (point.0 - current.0).abs();
            let dy = (point.1 - current.1).abs();
            self.has_diagonal |= dx > 0.25 && dy > 0.25;
            self.segment_count += 1;
        }
        self.point(point);
        self.current = Some(point);
    }

    fn curve_to(&mut self, controls_and_end: &[(f32, f32)]) {
        for point in controls_and_end {
            self.point(*point);
        }
        if let Some(end) = controls_and_end.last().copied() {
            self.current = Some(end);
        }
        self.segment_count += 1;
        self.has_curve = true;
    }

    fn close(&mut self) {
        if let Some(start) = self.start {
            self.line_to(start);
        }
    }

    fn paint(&mut self, page: u32, output: &mut Vec<PdfPaintedPath>) {
        if let Some(bbox) = self.bbox {
            let width = bbox[2] - bbox[0];
            let height = bbox[3] - bbox[1];
            if self.segment_count > 0 && width > 0.5 && height > 0.5 {
                output.push(PdfPaintedPath {
                    bbox,
                    page,
                    segment_count: self.segment_count,
                    has_curve: self.has_curve,
                    has_diagonal: self.has_diagonal,
                });
            }
        }
        *self = Self::default();
    }
}

fn point(op: &Operation, offset: usize, ctm: &[f32; 6]) -> Option<(f32, f32)> {
    let x = op.operands.get(offset).and_then(get_number)?;
    let y = op.operands.get(offset + 1).and_then(get_number)?;
    Some((
        x * ctm[0] + y * ctm[2] + ctm[4],
        x * ctm[1] + y * ctm[3] + ctm[5],
    ))
}

pub(crate) fn extract_painted_paths(
    doc: &Document,
    page_filter: Option<&HashSet<u32>>,
) -> Vec<PdfPaintedPath> {
    let mut output = Vec::new();
    for (page, page_id) in doc.get_pages() {
        if page_filter.is_some_and(|filter| !filter.contains(&page)) {
            continue;
        }
        let Ok(data) = doc.get_page_content(page_id) else {
            continue;
        };
        let Ok(content) = Content::decode(&data) else {
            continue;
        };
        let xobjects = get_page_xobjects(doc, page_id);
        walk_operations(
            doc,
            &content.operations,
            &xobjects,
            page,
            [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            0,
            &mut output,
        );
    }
    output
}

fn walk_form(
    doc: &Document,
    form_id: ObjectId,
    page: u32,
    parent_ctm: [f32; 6],
    depth: u8,
    output: &mut Vec<PdfPaintedPath>,
) {
    if depth > MAX_FORM_DEPTH {
        return;
    }
    let Ok(Object::Stream(stream)) = doc.get_object(form_id) else {
        return;
    };
    let data = stream
        .decompressed_content()
        .unwrap_or_else(|_| stream.content.clone());
    let Ok(content) = Content::decode(&data) else {
        return;
    };
    let matrix = stream
        .dict
        .get(b"Matrix")
        .ok()
        .and_then(|value| value.as_array().ok())
        .filter(|values| values.len() >= 6)
        .map(|values| {
            let mut matrix = [0.0; 6];
            for (index, value) in values.iter().take(6).enumerate() {
                matrix[index] =
                    get_number(value).unwrap_or(if index == 0 || index == 3 { 1.0 } else { 0.0 });
            }
            matrix
        })
        .unwrap_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    let xobjects = get_form_xobjects(doc, &stream.dict);
    walk_operations(
        doc,
        &content.operations,
        &xobjects,
        page,
        multiply_matrices(&matrix, &parent_ctm),
        depth,
        output,
    );
}

#[allow(clippy::too_many_arguments)]
fn walk_operations(
    doc: &Document,
    operations: &[Operation],
    xobjects: &HashMap<String, XObjectType>,
    page: u32,
    base_ctm: [f32; 6],
    depth: u8,
    output: &mut Vec<PdfPaintedPath>,
) {
    let mut ctm = base_ctm;
    let mut stack = Vec::new();
    let mut path = PathState::default();
    for op in operations {
        match op.operator.as_str() {
            "q" => stack.push(ctm),
            "Q" => {
                if let Some(saved) = stack.pop() {
                    ctm = saved;
                }
            }
            "cm" if op.operands.len() >= 6 => {
                let mut matrix = [0.0; 6];
                for (index, value) in op.operands.iter().take(6).enumerate() {
                    matrix[index] = get_number(value).unwrap_or(if index == 0 || index == 3 {
                        1.0
                    } else {
                        0.0
                    });
                }
                ctm = multiply_matrices(&matrix, &ctm);
            }
            "m" => {
                if let Some(point) = point(op, 0, &ctm) {
                    path.move_to(point);
                }
            }
            "l" => {
                if let Some(point) = point(op, 0, &ctm) {
                    path.line_to(point);
                }
            }
            "re" if op.operands.len() >= 4 => {
                let Some(x) = op.operands.first().and_then(get_number) else {
                    continue;
                };
                let Some(y) = op.operands.get(1).and_then(get_number) else {
                    continue;
                };
                let Some(width) = op.operands.get(2).and_then(get_number) else {
                    continue;
                };
                let Some(height) = op.operands.get(3).and_then(get_number) else {
                    continue;
                };
                let transform = |px: f32, py: f32| {
                    (
                        px * ctm[0] + py * ctm[2] + ctm[4],
                        px * ctm[1] + py * ctm[3] + ctm[5],
                    )
                };
                path.move_to(transform(x, y));
                path.line_to(transform(x + width, y));
                path.line_to(transform(x + width, y + height));
                path.line_to(transform(x, y + height));
                path.close();
            }
            "c" if op.operands.len() >= 6 => {
                if let (Some(a), Some(b), Some(end)) =
                    (point(op, 0, &ctm), point(op, 2, &ctm), point(op, 4, &ctm))
                {
                    path.curve_to(&[a, b, end]);
                }
            }
            "v" if op.operands.len() >= 4 => {
                if let (Some(control), Some(end)) = (point(op, 0, &ctm), point(op, 2, &ctm)) {
                    path.curve_to(&[control, end]);
                }
            }
            "y" if op.operands.len() >= 4 => {
                if let (Some(control), Some(end)) = (point(op, 0, &ctm), point(op, 2, &ctm)) {
                    path.curve_to(&[control, end]);
                }
            }
            "h" => path.close(),
            "s" | "b" | "b*" => {
                path.close();
                path.paint(page, output);
            }
            "S" | "B" | "B*" | "f" | "F" | "f*" => path.paint(page, output),
            "n" => path = PathState::default(),
            "Do" if depth < MAX_FORM_DEPTH => {
                let Some(name) = op
                    .operands
                    .first()
                    .and_then(|value| value.as_name().ok())
                    .map(|name| String::from_utf8_lossy(name).to_string())
                else {
                    continue;
                };
                if let Some(XObjectType::Form(form_id)) = xobjects.get(&name) {
                    walk_form(doc, *form_id, page, ctm, depth + 1, output);
                }
            }
            _ => {}
        }
    }
}

fn boxes_near(a: [f32; 4], b: [f32; 4], gap: f32) -> bool {
    a[0] <= b[2] + gap && a[2] + gap >= b[0] && a[1] <= b[3] + gap && a[3] + gap >= b[1]
}

fn union(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [
        a[0].min(b[0]),
        a[1].min(b[1]),
        a[2].max(b[2]),
        a[3].max(b[3]),
    ]
}

fn is_numeric_value(text: &str) -> bool {
    let mut has_digit = false;
    for character in text.trim().chars() {
        if character.is_ascii_digit() {
            has_digit = true;
        } else if !matches!(
            character,
            ' ' | '\t' | ',' | '.' | '-' | '+' | '(' | ')' | '%' | '$' | '¥' | '￥' | '€' | '£'
        ) {
            return false;
        }
    }
    has_digit
}

fn clustered_axis_count(mut values: Vec<f32>, tolerance: f32, minimum_members: usize) -> usize {
    values.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let mut clusters = Vec::<Vec<f32>>::new();
    for value in values {
        if let Some(cluster) = clusters.iter_mut().find(|cluster| {
            (value - cluster.iter().sum::<f32>() / cluster.len() as f32).abs() <= tolerance
        }) {
            cluster.push(value);
        } else {
            clusters.push(vec![value]);
        }
    }
    clusters
        .iter()
        .filter(|cluster| cluster.len() >= minimum_members)
        .count()
}

fn is_numeric_column_table(items: &[TextItem], page: u32, bbox: [f32; 4]) -> bool {
    let numeric: Vec<&TextItem> = items
        .iter()
        .filter(|item| item.page == page && matches!(item.item_type, ItemType::Text))
        .filter(|item| {
            let center_x = item.x + item.width / 2.0;
            let center_y = item.y + item.height / 2.0;
            center_x >= bbox[0] && center_x <= bbox[2] && center_y >= bbox[1] && center_y <= bbox[3]
        })
        .filter(|item| is_numeric_value(&item.text))
        .collect();
    if numeric.len() < 12 {
        return false;
    }
    let stable_numeric_columns = clustered_axis_count(
        numeric.iter().map(|item| item.x + item.width).collect(),
        8.0,
        6,
    );
    let populated_rows = clustered_axis_count(numeric.iter().map(|item| item.y).collect(), 3.0, 1);
    stable_numeric_columns >= 1 && populated_rows >= 8
}

/// Detect vector figures from painted paths. The gates deliberately reject
/// table grids, page frames, separators, and swarms of tiny outlined glyphs.
pub(crate) fn detect_vector_figure_regions(
    paths: &[PdfPaintedPath],
    items: &[TextItem],
    page: u32,
    page_size: [f32; 2],
) -> Vec<[f32; 4]> {
    let candidates: Vec<&PdfPaintedPath> = paths
        .iter()
        .filter(|path| {
            path.page == page && (path.has_curve || path.has_diagonal || path.segment_count >= 3)
        })
        .filter(|path| {
            let [x0, y0, x1, y1] = path.bbox;
            let width = x1 - x0;
            let height = y1 - y0;
            width >= 1.5
                && height >= 1.5
                && width <= page_size[0] * 0.96
                && height <= page_size[1] * 0.96
        })
        .take(20_000)
        .collect();
    if candidates.is_empty() {
        return Vec::new();
    }
    log::debug!(
        "page {page}: {} painted vector-path candidates",
        candidates.len()
    );

    let mut parent: Vec<usize> = (0..candidates.len()).collect();
    fn find(parent: &mut [usize], mut index: usize) -> usize {
        while parent[index] != index {
            parent[index] = parent[parent[index]];
            index = parent[index];
        }
        index
    }
    fn join(parent: &mut [usize], left: usize, right: usize) {
        let left = find(parent, left);
        let right = find(parent, right);
        if left != right {
            parent[right] = left;
        }
    }
    let mut order: Vec<usize> = (0..candidates.len()).collect();
    order.sort_by(|&left, &right| {
        candidates[left].bbox[0]
            .partial_cmp(&candidates[right].bbox[0])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for (position, &left) in order.iter().enumerate() {
        let right_edge = candidates[left].bbox[2] + 10.0;
        for &right in order.iter().skip(position + 1) {
            if candidates[right].bbox[0] > right_edge {
                break;
            }
            if boxes_near(candidates[left].bbox, candidates[right].bbox, 10.0) {
                join(&mut parent, left, right);
            }
        }
    }
    let mut components: HashMap<usize, Vec<usize>> = HashMap::new();
    for index in 0..candidates.len() {
        let root = find(&mut parent, index);
        components.entry(root).or_default().push(index);
    }
    let mut regions = Vec::new();
    for component in components.values() {
        let bbox = component
            .iter()
            .map(|&index| candidates[index].bbox)
            .reduce(union)
            .unwrap();
        let width = bbox[2] - bbox[0];
        let height = bbox[3] - bbox[1];
        let segments: u32 = component
            .iter()
            .map(|&index| candidates[index].segment_count)
            .sum();
        let large_paths = component
            .iter()
            .filter(|&&index| {
                let path = candidates[index];
                path.bbox[2] - path.bbox[0] >= 36.0 || path.bbox[3] - path.bbox[1] >= 24.0
            })
            .count();
        let curved = component
            .iter()
            .filter(|&&index| candidates[index].has_curve)
            .count();
        let diagonal = component
            .iter()
            .filter(|&&index| candidates[index].has_diagonal)
            .count();
        let page_frame = bbox[0] <= 5.0
            && bbox[1] <= 5.0
            && width >= page_size[0] * 0.85
            && height >= page_size[1] * 0.85;
        let outlined_glyph_swarm = component.len() >= 64 && large_paths == 0;
        let mostly_uniform = |mut values: Vec<f32>| {
            if values.is_empty() {
                return false;
            }
            values.sort_by(|left, right| left.total_cmp(right));
            let median = values[values.len() / 2];
            let tolerance = (median.abs() * 0.12).max(1.0);
            values
                .iter()
                .filter(|&&value| (value - median).abs() <= tolerance)
                .count()
                * 5
                >= values.len() * 4
        };
        let uniform_axis_grid = component.len() >= 6
            && curved == 0
            && diagonal == 0
            && mostly_uniform(
                component
                    .iter()
                    .map(|&index| candidates[index].bbox[2] - candidates[index].bbox[0])
                    .collect(),
            )
            && mostly_uniform(
                component
                    .iter()
                    .map(|&index| candidates[index].bbox[3] - candidates[index].bbox[1])
                    .collect(),
            );
        let complex_enough = large_paths >= 1 && (segments >= 3 || curved >= 2)
            || large_paths >= 2
            || (component.len() >= 3 && segments >= 6);
        let region_text_items: Vec<&TextItem> = items
            .iter()
            .filter(|item| item.page == page && matches!(item.item_type, ItemType::Text))
            .filter(|item| {
                let center_x = item.x + item.width / 2.0;
                let center_y = item.y + item.height / 2.0;
                center_x >= bbox[0]
                    && center_x <= bbox[2]
                    && center_y >= bbox[1]
                    && center_y <= bbox[3]
            })
            .collect();
        let text_chars: usize = region_text_items
            .iter()
            .map(|item| item.text.trim().chars().count())
            .sum();
        let text_density = text_chars as f32 / (width * height).max(1.0);
        let prose_panel = component.len() <= 2 && text_chars >= 600 && text_density >= 0.003;
        let numeric_table =
            curved == 0 && diagonal == 0 && is_numeric_column_table(items, page, bbox);
        let long_text_items = region_text_items
            .iter()
            .filter(|item| {
                item.text
                    .chars()
                    .filter(|character| character.is_alphabetic())
                    .count()
                    >= 20
            })
            .count();
        let text_rows = clustered_axis_count(
            region_text_items.iter().map(|item| item.y).collect(),
            3.0,
            1,
        );
        let axis_text_schedule =
            curved == 0 && diagonal == 0 && long_text_items >= 4 && text_rows >= 8;
        log::trace!(
            "page {page}: vector component bbox={bbox:?} paths={} segments={segments} curves={curved} large={large_paths} chars={text_chars} density={text_density:.4}",
            component.len()
        );
        if width >= 72.0
            && height >= 36.0
            && width * height >= 4_000.0
            && complex_enough
            && !page_frame
            && !outlined_glyph_swarm
            && !uniform_axis_grid
            && !prose_panel
            && !numeric_table
            && !axis_text_schedule
        {
            regions.push(bbox);
        }
    }
    regions
}

/// Merge overlapping detections from independent geometry strategies.
pub(crate) fn merge_figure_regions(mut regions: Vec<[f32; 4]>) -> Vec<[f32; 4]> {
    let mut changed = true;
    while changed {
        changed = false;
        'outer: for left in 0..regions.len() {
            for right in left + 1..regions.len() {
                if boxes_near(regions[left], regions[right], 24.0) {
                    let merged = union(regions[left], regions[right]);
                    regions[left] = merged;
                    regions.swap_remove(right);
                    changed = true;
                    break 'outer;
                }
            }
        }
    }
    regions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(bbox: [f32; 4], segments: u32, curve: bool, diagonal: bool) -> PdfPaintedPath {
        PdfPaintedPath {
            bbox,
            page: 1,
            segment_count: segments,
            has_curve: curve,
            has_diagonal: diagonal,
        }
    }

    fn text_item(text: String, x: f32, y: f32) -> TextItem {
        TextItem {
            text,
            x,
            y,
            width: 45.0,
            height: 10.0,
            font: String::new(),
            font_size: 10.0,
            page: 1,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            item_type: ItemType::Text,
            mcid: None,
        }
    }

    #[test]
    fn curved_plot_is_detected_without_caption_or_page_rules() {
        let paths = vec![path([80.0, 180.0, 460.0, 420.0], 8, true, true)];
        assert_eq!(
            detect_vector_figure_regions(&paths, &[], 1, [612.0, 792.0]).len(),
            1
        );
    }

    #[test]
    fn filled_axis_aligned_bar_cluster_is_detected() {
        let paths: Vec<_> = (0..6)
            .map(|index| {
                let x = 80.0 + index as f32 * 28.0;
                path(
                    [x, 180.0, x + 20.0, 230.0 + index as f32 * 18.0],
                    4,
                    false,
                    false,
                )
            })
            .collect();
        assert_eq!(
            detect_vector_figure_regions(&paths, &[], 1, [612.0, 792.0]).len(),
            1
        );
    }

    #[test]
    fn uniform_closed_rectangle_grid_is_rejected() {
        let paths: Vec<_> = (0..4)
            .flat_map(|row| {
                (0..3).map(move |column| {
                    let x = 80.0 + column as f32 * 90.0;
                    let y = 180.0 + row as f32 * 24.0;
                    path([x, y, x + 90.0, y + 24.0], 4, false, false)
                })
            })
            .collect();
        assert!(detect_vector_figure_regions(&paths, &[], 1, [612.0, 792.0]).is_empty());
    }

    #[test]
    fn numeric_columns_inside_a_page_frame_are_rejected_as_a_table() {
        let paths = vec![path([40.0, 100.0, 570.0, 700.0], 12, false, false)];
        let mut items = Vec::new();
        for row in 0..12 {
            let y = 150.0 + row as f32 * 35.0;
            items.push(text_item(format!("{},000.00", row + 1), 390.0, y));
            items.push(text_item(format!("{},500.00", row + 1), 500.0, y));
        }
        assert!(detect_vector_figure_regions(&paths, &items, 1, [612.0, 792.0]).is_empty());
    }

    #[test]
    fn axis_aligned_multiline_schedule_is_not_a_figure() {
        let paths = vec![path([40.0, 100.0, 570.0, 700.0], 12, false, false)];
        let items: Vec<_> = (0..10)
            .map(|row| {
                text_item(
                    format!("Long accounting description for record {row}"),
                    70.0 + row as f32,
                    150.0 + row as f32 * 35.0,
                )
            })
            .collect();
        assert!(detect_vector_figure_regions(&paths, &items, 1, [612.0, 792.0]).is_empty());
    }

    #[test]
    fn table_grid_and_page_frame_are_rejected() {
        let paths = vec![
            path([0.0, 0.0, 612.0, 792.0], 4, false, true),
            path([80.0, 180.0, 460.0, 181.0], 1, false, false),
        ];
        assert!(detect_vector_figure_regions(&paths, &[], 1, [612.0, 792.0]).is_empty());
    }

    #[test]
    fn tiny_outlined_glyph_swarm_is_rejected() {
        let paths: Vec<_> = (0..80)
            .map(|index| {
                let x = 50.0 + (index % 20) as f32 * 8.0;
                let y = 200.0 + (index / 20) as f32 * 9.0;
                path([x, y, x + 6.0, y + 7.0], 4, true, false)
            })
            .collect();
        assert!(detect_vector_figure_regions(&paths, &[], 1, [612.0, 792.0]).is_empty());
    }

    #[test]
    fn rounded_prose_panel_is_rejected_by_text_mass() {
        let paths = vec![path([40.0, 300.0, 570.0, 740.0], 8, true, true)];
        let items: Vec<_> = (0..8)
            .map(|row| {
                let mut item = text_item(
                    "ordinary paragraph text ".repeat(5),
                    70.0,
                    330.0 + row as f32 * 35.0,
                );
                item.width = 450.0;
                item
            })
            .collect();
        assert!(detect_vector_figure_regions(&paths, &items, 1, [612.0, 792.0]).is_empty());
    }

    #[test]
    fn disjoint_regions_remain_separate() {
        let regions = merge_figure_regions(vec![
            [20.0, 20.0, 120.0, 80.0],
            [121.0, 20.0, 220.0, 80.0],
            [400.0, 500.0, 500.0, 600.0],
        ]);
        assert_eq!(regions.len(), 2);
    }

    #[test]
    fn content_stream_collects_only_painted_cubic_paths() {
        let content =
            Content::decode(b"10 20 m 40 80 160 80 200 20 c S 10 10 m 20 30 30 30 40 10 c n")
                .unwrap();
        let mut paths = Vec::new();
        walk_operations(
            &Document::new(),
            &content.operations,
            &HashMap::new(),
            1,
            [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            0,
            &mut paths,
        );
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].bbox, [10.0, 20.0, 200.0, 80.0]);
        assert!(paths[0].has_curve);
    }

    #[test]
    fn rectangle_operator_requires_a_paint_operation() {
        let content = Content::decode(b"10 20 100 60 re f 200 20 100 60 re W n").unwrap();
        let mut paths = Vec::new();
        walk_operations(
            &Document::new(),
            &content.operations,
            &HashMap::new(),
            1,
            [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            0,
            &mut paths,
        );
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].bbox, [10.0, 20.0, 110.0, 80.0]);
    }
}
