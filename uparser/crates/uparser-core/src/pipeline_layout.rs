//! Rust preprocessing and raw-output decoding for PP-DocLayoutV2.

use crate::tensor_wire::{Tensor, TensorBundle, TensorDType, TensorWireError};
use std::collections::BTreeMap;

pub const LABELS: [&str; 25] = [
    "abstract",
    "algorithm",
    "aside_text",
    "chart",
    "content",
    "display_formula",
    "doc_title",
    "figure_title",
    "footer",
    "footer_image",
    "footnote",
    "formula_number",
    "header",
    "header_image",
    "image",
    "inline_formula",
    "number",
    "paragraph_title",
    "reference",
    "reference_content",
    "seal",
    "table",
    "text",
    "vertical_text",
    "vision_footnote",
];

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct LayoutDetection {
    pub label_id: usize,
    pub label: &'static str,
    pub score: f32,
    pub bbox: [f32; 4],
    pub order: usize,
}

fn cubic_weight(distance: f32) -> f32 {
    // PyTorch bicubic interpolation uses a = -0.75 when antialias is disabled.
    let distance = distance.abs();
    let a = -0.75f32;
    if distance <= 1.0 {
        ((a + 2.0) * distance - (a + 3.0)) * distance * distance + 1.0
    } else if distance < 2.0 {
        ((a * distance - 5.0 * a) * distance + 8.0 * a) * distance - 4.0 * a
    } else {
        0.0
    }
}

fn resize_bicubic_torchvision(source: &image::RgbImage, width: u32, height: u32) -> Vec<u8> {
    let (source_width, source_height) = source.dimensions();
    let x_scale = source_width as f32 / width as f32;
    let y_scale = source_height as f32 / height as f32;
    let x_weights: Vec<[(usize, f32); 4]> = (0..width)
        .map(|x| {
            let source = x_scale * (x as f32 + 0.5) - 0.5;
            let base = source.floor() as i64;
            std::array::from_fn(|offset| {
                let index = base + offset as i64 - 1;
                (
                    index.clamp(0, i64::from(source_width) - 1) as usize,
                    cubic_weight(source - index as f32),
                )
            })
        })
        .collect();
    let y_weights: Vec<[(usize, f32); 4]> = (0..height)
        .map(|y| {
            let source = y_scale * (y as f32 + 0.5) - 0.5;
            let base = source.floor() as i64;
            std::array::from_fn(|offset| {
                let index = base + offset as i64 - 1;
                (
                    index.clamp(0, i64::from(source_height) - 1) as usize,
                    cubic_weight(source - index as f32),
                )
            })
        })
        .collect();

    let mut output = vec![0u8; width as usize * height as usize * 3];
    for (y, y_samples) in y_weights.iter().enumerate() {
        for (x, x_samples) in x_weights.iter().enumerate() {
            for channel in 0..3 {
                let mut value = 0.0f32;
                for &(source_y, y_weight) in y_samples {
                    let mut row = 0.0f32;
                    for &(source_x, x_weight) in x_samples {
                        row +=
                            f32::from(source.get_pixel(source_x as u32, source_y as u32)[channel])
                                * x_weight;
                    }
                    value += row * y_weight;
                }
                output[(y * width as usize + x) * 3 + channel] =
                    value.round_ties_even().clamp(0.0, 255.0) as u8;
            }
        }
    }
    output
}

#[derive(Debug, thiserror::Error)]
pub enum LayoutError {
    #[error("cannot decode page image: {0}")]
    Image(#[from] image::ImageError),
    #[error(transparent)]
    Tensor(#[from] TensorWireError),
    #[error("missing layout output tensor: {0}")]
    MissingTensor(&'static str),
    #[error("layout tensor {name} has shape {actual:?}, expected {expected}")]
    InvalidShape {
        name: &'static str,
        actual: Vec<usize>,
        expected: &'static str,
    },
}

pub fn preprocess(
    encoded_image: &[u8],
    target_width: u32,
    target_height: u32,
) -> Result<(TensorBundle, (u32, u32)), LayoutError> {
    let source = image::load_from_memory(encoded_image)?.to_rgb8();
    let original = source.dimensions();
    let resized = resize_bicubic_torchvision(&source, target_width, target_height);
    let pixel_count = (target_width as usize) * (target_height as usize);
    let mut chw = vec![0.0f32; pixel_count * 3];
    for (index, pixel) in resized.chunks_exact(3).enumerate() {
        chw[index] = f32::from(pixel[0]) / 255.0;
        chw[pixel_count + index] = f32::from(pixel[1]) / 255.0;
        chw[pixel_count * 2 + index] = f32::from(pixel[2]) / 255.0;
    }
    Ok((
        TensorBundle {
            metadata: BTreeMap::from([
                ("model".into(), "pp_doclayout_v2".into()),
                ("original_width".into(), original.0.to_string()),
                ("original_height".into(), original.1.to_string()),
            ]),
            tensors: vec![Tensor::from_f32(
                "pixel_values",
                vec![1, 3, target_height as usize, target_width as usize],
                &chw,
            )],
        },
        original,
    ))
}

fn tensor<'a>(bundle: &'a TensorBundle, name: &'static str) -> Result<&'a Tensor, LayoutError> {
    bundle
        .tensors
        .iter()
        .find(|tensor| tensor.name == name)
        .ok_or(LayoutError::MissingTensor(name))
}

fn f32_tensor<'a>(
    bundle: &'a TensorBundle,
    name: &'static str,
) -> Result<(&'a [usize], Vec<f32>), LayoutError> {
    let tensor = tensor(bundle, name)?;
    if tensor.dtype != TensorDType::F32 {
        return Err(LayoutError::Tensor(TensorWireError::UnexpectedDType {
            name: name.into(),
            expected: TensorDType::F32,
            actual: tensor.dtype,
        }));
    }
    Ok((&tensor.shape, tensor.to_f32()?))
}

fn sigmoid(value: f32) -> f32 {
    if value >= 0.0 {
        1.0 / (1.0 + (-value).exp())
    } else {
        let exponential = value.exp();
        exponential / (1.0 + exponential)
    }
}

fn area(bbox: [f32; 4]) -> f32 {
    (bbox[2] - bbox[0]).max(0.0) * (bbox[3] - bbox[1]).max(0.0)
}

fn intersection_area(left: [f32; 4], right: [f32; 4]) -> f32 {
    area([
        left[0].max(right[0]),
        left[1].max(right[1]),
        left[2].min(right[2]),
        left[3].min(right[3]),
    ])
}

fn overlap_ratio(left: [f32; 4], right: [f32; 4]) -> f32 {
    let reference = area(left).min(area(right));
    if reference <= 0.0 {
        0.0
    } else {
        intersection_area(left, right) / reference
    }
}

fn iou(left: [f32; 4], right: [f32; 4]) -> f32 {
    let intersection = intersection_area(left, right);
    let union = area(left) + area(right) - intersection;
    if union <= 0.0 {
        0.0
    } else {
        intersection / union
    }
}

fn x_overlap_ratio(left: [f32; 4], right: [f32; 4]) -> f32 {
    let reference = (left[2] - left[0])
        .max(0.0)
        .min((right[2] - right[0]).max(0.0));
    if reference <= 0.0 {
        return 0.0;
    }
    (left[2].min(right[2]) - left[0].max(right[0])).max(0.0) / reference
}

fn x_cover_ratio(anchor: [f32; 4], candidate: [f32; 4]) -> f32 {
    let width = (candidate[2] - candidate[0]).max(0.0);
    if width <= 0.0 {
        return 0.0;
    }
    (anchor[2].min(candidate[2]) - anchor[0].max(candidate[0])).max(0.0) / width
}

fn is_formula(detection: &LayoutDetection) -> bool {
    matches!(detection.label, "display_formula" | "inline_formula")
}

fn set_label(detection: &mut LayoutDetection, label_id: usize) {
    detection.label_id = label_id;
    detection.label = LABELS[label_id];
}

fn renumber(detections: &mut [LayoutDetection]) {
    for (index, detection) in detections.iter_mut().enumerate() {
        detection.order = index + 1;
    }
}

fn paddlex_filter(mut detections: Vec<LayoutDetection>) -> Vec<LayoutDetection> {
    detections.retain(|detection| detection.label != "reference");
    let mut dropped = vec![false; detections.len()];
    for left in 0..detections.len() {
        if dropped[left] {
            continue;
        }
        let bbox = detections[left].bbox;
        if (bbox[2] - bbox[0] < 6.0 || bbox[3] - bbox[1] < 6.0)
            && detections[left].label != "inline_formula"
        {
            dropped[left] = true;
            continue;
        }
        for right in (left + 1)..detections.len() {
            if dropped[left] || dropped[right] {
                continue;
            }
            if detections[left].label == "inline_formula"
                || detections[right].label == "inline_formula"
            {
                continue;
            }
            if overlap_ratio(detections[left].bbox, detections[right].bbox) <= 0.7 {
                continue;
            }
            let left_label = detections[left].label;
            let right_label = detections[right].label;
            let protected = |label| matches!(label, "image" | "table" | "seal" | "chart");
            if left_label != right_label && (protected(left_label) || protected(right_label)) {
                let both_protected = protected(left_label) && protected(right_label);
                if left_label != "table" && right_label != "table" || both_protected {
                    continue;
                }
            }
            if area(detections[left].bbox) >= area(detections[right].bbox) {
                dropped[right] = true;
            } else {
                dropped[left] = true;
            }
        }
    }
    let mut kept: Vec<_> = detections
        .into_iter()
        .zip(dropped)
        .filter_map(|(detection, dropped)| (!dropped).then_some(detection))
        .collect();
    renumber(&mut kept);
    kept
}

fn deduplicate(mut detections: Vec<LayoutDetection>) -> Vec<LayoutDetection> {
    let mut candidates: Vec<usize> = (0..detections.len()).collect();
    candidates.sort_by(|left, right| {
        detections[*right]
            .score
            .total_cmp(&detections[*left].score)
            .then_with(|| left.cmp(right))
    });
    let mut dropped = vec![false; detections.len()];
    let mut kept = Vec::new();
    for (position, &current) in candidates.iter().enumerate() {
        if dropped[current] {
            continue;
        }
        kept.push(current);
        for &other in &candidates[position + 1..] {
            if !dropped[other] && iou(detections[current].bbox, detections[other].bbox) > 0.9 {
                dropped[other] = true;
            }
        }
    }
    kept.sort_unstable();
    detections = kept
        .into_iter()
        .map(|index| detections[index].clone())
        .collect();
    detections
}

fn merge_nested_formulas(mut detections: Vec<LayoutDetection>) -> Vec<LayoutDetection> {
    loop {
        let formula_indices: Vec<usize> = detections
            .iter()
            .enumerate()
            .filter_map(|(index, detection)| is_formula(detection).then_some(index))
            .collect();
        let mut pair = None;
        'outer: for (position, &left) in formula_indices.iter().enumerate() {
            for &right in &formula_indices[position + 1..] {
                if overlap_ratio(detections[left].bbox, detections[right].bbox) >= 0.7 {
                    pair = Some((left, right));
                    break 'outer;
                }
            }
        }
        let Some((left, right)) = pair else { break };
        let left_area = area(detections[left].bbox);
        let right_area = area(detections[right].bbox);
        let (keep, drop) = if left_area > right_area
            || (left_area == right_area && detections[left].score >= detections[right].score)
        {
            (left, right)
        } else {
            (right, left)
        };
        let dropped_bbox = detections[drop].bbox;
        detections[keep].bbox = [
            detections[keep].bbox[0].min(dropped_bbox[0]),
            detections[keep].bbox[1].min(dropped_bbox[1]),
            detections[keep].bbox[2].max(dropped_bbox[2]),
            detections[keep].bbox[3].max(dropped_bbox[3]),
        ];
        detections[keep].score = detections[keep].score.max(detections[drop].score);
        detections.remove(drop);
    }
    detections
}

fn post_process(
    mut detections: Vec<LayoutDetection>,
    page_width: f32,
    page_height: f32,
) -> Vec<LayoutDetection> {
    detections = merge_nested_formulas(deduplicate(detections));

    let parents: Vec<[f32; 4]> = detections
        .iter()
        .filter(|detection| {
            !is_formula(detection)
                && detection.label != "formula_number"
                && detection.label != "reference"
        })
        .map(|detection| detection.bbox)
        .collect();
    for detection in &mut detections {
        if is_formula(detection) {
            let inline = parents.iter().any(|parent| {
                intersection_area(detection.bbox, *parent) / area(detection.bbox) >= 0.7
            });
            set_label(detection, if inline { 15 } else { 5 });
        }
    }

    for detection in &mut detections {
        let upper = (detection.bbox[1] + detection.bbox[3]) * 0.5 < page_height * 0.5;
        match (upper, detection.label) {
            (true, "footer") => set_label(detection, 12),
            (true, "footer_image") => set_label(detection, 13),
            (false, "header") => set_label(detection, 8),
            (false, "header_image") => set_label(detection, 9),
            _ => {}
        }
    }

    let header_anchor = detections
        .iter()
        .enumerate()
        .filter(|(_, detection)| matches!(detection.label, "header" | "header_image"))
        .max_by(|(_, left), (_, right)| {
            left.bbox[3]
                .total_cmp(&right.bbox[3])
                .then(left.order.cmp(&right.order))
        })
        .map(|(index, _)| index);
    let footer_anchor = detections
        .iter()
        .enumerate()
        .filter(|(_, detection)| matches!(detection.label, "footer" | "footer_image"))
        .min_by(|(_, left), (_, right)| {
            left.bbox[1]
                .total_cmp(&right.bbox[1])
                .then(left.order.cmp(&right.order))
        })
        .map(|(index, _)| index);
    let boundary_candidate = |label: &str| !matches!(label, "aside_text" | "footnote" | "number");
    if let Some(anchor) = header_anchor {
        let boundary = detections[anchor].bbox[3];
        for detection in &mut detections {
            if boundary_candidate(detection.label)
                && !matches!(detection.label, "header" | "header_image")
                && detection.bbox[3] <= boundary
            {
                set_label(detection, 12);
            }
        }
    }
    let footnotes: Vec<[f32; 4]> = detections
        .iter()
        .filter(|detection| detection.label == "footnote")
        .map(|detection| detection.bbox)
        .collect();
    for detection in &mut detections {
        if !matches!(
            detection.label,
            "header"
                | "header_image"
                | "footer"
                | "footer_image"
                | "footnote"
                | "number"
                | "aside_text"
        ) && footnotes.iter().any(|footnote| {
            detection.bbox[1] >= footnote[1] && x_cover_ratio(*footnote, detection.bbox) >= 0.7
        }) {
            set_label(detection, 10);
        }
    }
    if let Some(anchor) = footer_anchor {
        let anchor_bbox = detections[anchor].bbox;
        for detection in &mut detections {
            let full_width = (anchor_bbox[2] - anchor_bbox[0]) / page_width >= 0.7;
            if boundary_candidate(detection.label)
                && !matches!(detection.label, "footer" | "footer_image")
                && detection.bbox[1] >= anchor_bbox[1]
                && (full_width || x_overlap_ratio(anchor_bbox, detection.bbox) >= 0.3)
            {
                set_label(detection, 8);
            }
        }
    }

    let top_number = detections
        .iter()
        .filter(|detection| {
            detection.label == "number"
                && (detection.bbox[1] + detection.bbox[3]) * 0.5 <= page_height * 0.3
        })
        .max_by(|left, right| {
            left.bbox[3]
                .total_cmp(&right.bbox[3])
                .then(left.order.cmp(&right.order))
        })
        .map(|detection| detection.bbox[1]);
    let bottom_number = detections
        .iter()
        .filter(|detection| {
            detection.label == "number"
                && (detection.bbox[1] + detection.bbox[3]) * 0.5 >= page_height * 0.7
        })
        .min_by(|left, right| {
            left.bbox[1]
                .total_cmp(&right.bbox[1])
                .then(left.order.cmp(&right.order))
        })
        .map(|detection| detection.bbox[3]);
    if let Some(boundary) = top_number {
        for detection in &mut detections {
            if boundary_candidate(detection.label) && detection.bbox[3] <= boundary {
                set_label(detection, 12);
            }
        }
    }
    if let Some(boundary) = bottom_number {
        for detection in &mut detections {
            if boundary_candidate(detection.label) && detection.bbox[1] >= boundary {
                set_label(detection, 8);
            }
        }
    }
    renumber(&mut detections);
    detections
}

fn order_ranks(order_logits: &[f32], queries: usize) -> Vec<usize> {
    let score = |row: usize, column: usize| sigmoid(order_logits[row * queries + column]);
    let mut votes = vec![0.0f32; queries];
    for column in 0..queries {
        votes[column] += (0..column).map(|row| score(row, column)).sum::<f32>();
        votes[column] += ((column + 1)..queries)
            .map(|row| 1.0 - score(column, row))
            .sum::<f32>();
    }
    let mut pointers: Vec<usize> = (0..queries).collect();
    pointers.sort_by(|left, right| votes[*left].total_cmp(&votes[*right]));
    let mut ranks = vec![0usize; queries];
    for (rank, query) in pointers.into_iter().enumerate() {
        ranks[query] = rank;
    }
    ranks
}

pub fn decode(
    bundle: &TensorBundle,
    original_width: u32,
    original_height: u32,
    confidence: f32,
) -> Result<Vec<LayoutDetection>, LayoutError> {
    let (logits_shape, logits) = f32_tensor(bundle, "logits")?;
    let (boxes_shape, boxes) = f32_tensor(bundle, "pred_boxes")?;
    let (order_shape, order) = f32_tensor(bundle, "order_logits")?;
    if logits_shape.len() != 3 || logits_shape[0] != 1 || logits_shape[2] != LABELS.len() {
        return Err(LayoutError::InvalidShape {
            name: "logits",
            actual: logits_shape.to_vec(),
            expected: "[1,queries,25]",
        });
    }
    let queries = logits_shape[1];
    if boxes_shape != [1, queries, 4] {
        return Err(LayoutError::InvalidShape {
            name: "pred_boxes",
            actual: boxes_shape.to_vec(),
            expected: "[1,queries,4]",
        });
    }
    if order_shape != [1, queries, queries] {
        return Err(LayoutError::InvalidShape {
            name: "order_logits",
            actual: order_shape.to_vec(),
            expected: "[1,queries,queries]",
        });
    }

    let ranks = order_ranks(&order, queries);
    let mut candidates: Vec<(f32, usize, usize)> = logits
        .chunks_exact(LABELS.len())
        .enumerate()
        .flat_map(|(query, values)| {
            values
                .iter()
                .enumerate()
                .map(move |(label, value)| (sigmoid(*value), query, label))
        })
        .collect();
    candidates.sort_by(|left, right| right.0.total_cmp(&left.0));
    candidates.truncate(queries);

    let width = original_width as f32;
    let height = original_height as f32;
    let mut detections = Vec::new();
    for (score, query, label_id) in candidates {
        if score < confidence {
            continue;
        }
        let [cx, cy, box_width, box_height]: [f32; 4] = boxes[query * 4..query * 4 + 4]
            .try_into()
            .expect("validated box shape");
        let bbox = [
            ((cx - box_width * 0.5) * width).clamp(0.0, width),
            ((cy - box_height * 0.5) * height).clamp(0.0, height),
            ((cx + box_width * 0.5) * width).clamp(0.0, width),
            ((cy + box_height * 0.5) * height).clamp(0.0, height),
        ];
        if bbox[2].ceil() <= bbox[0].floor() || bbox[3].ceil() <= bbox[1].floor() {
            continue;
        }
        detections.push(LayoutDetection {
            label_id,
            label: LABELS[label_id],
            score: (score * 10_000.0).round() / 10_000.0,
            bbox: [
                bbox[0].floor(),
                bbox[1].floor(),
                bbox[2].ceil(),
                bbox[3].ceil(),
            ],
            order: ranks[query],
        });
    }
    detections.sort_by_key(|detection| detection.order);
    Ok(post_process(
        paddlex_filter(detections),
        original_width as f32,
        original_height as f32,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detection(label_id: usize, score: f32, bbox: [f32; 4], order: usize) -> LayoutDetection {
        LayoutDetection {
            label_id,
            label: LABELS[label_id],
            score,
            bbox,
            order,
        }
    }

    #[test]
    fn bicubic_resize_stays_within_torchvision_uint8_kernel_tolerance() {
        let source = image::RgbImage::from_raw(
            2,
            2,
            vec![0, 10, 20, 64, 70, 80, 128, 130, 140, 255, 245, 235],
        )
        .unwrap();
        // Produced by torchvision 0.21 / PyTorch 2.6 tensor resize with
        // bicubic interpolation and antialias disabled.
        let torchvision: [u8; 27] = [
            0, 0, 5, 18, 27, 38, 53, 60, 71, 56, 62, 73, 112, 114, 119, 168, 165, 164, 128, 130,
            142, 205, 200, 199, 255, 255, 255,
        ];
        let rust = resize_bicubic_torchvision(&source, 3, 3);
        assert_eq!(rust, torchvision);
    }

    #[test]
    fn paddlex_filter_drops_nested_duplicate_but_preserves_inline_formula() {
        let filtered = paddlex_filter(vec![
            detection(17, 0.8, [10.0, 10.0, 110.0, 40.0], 1),
            detection(22, 0.9, [11.0, 10.0, 109.0, 40.0], 2),
            detection(15, 0.7, [20.0, 15.0, 40.0, 25.0], 3),
        ]);
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].label, "paragraph_title");
        assert_eq!(filtered[1].label, "inline_formula");
    }

    #[test]
    fn post_process_relabels_nested_formula_and_page_half_anchors() {
        let processed = post_process(
            vec![
                detection(8, 0.9, [0.0, 10.0, 100.0, 30.0], 1),
                detection(22, 0.9, [10.0, 100.0, 200.0, 150.0], 2),
                detection(5, 0.9, [20.0, 110.0, 50.0, 130.0], 3),
                detection(12, 0.9, [0.0, 900.0, 100.0, 930.0], 4),
            ],
            500.0,
            1000.0,
        );
        assert_eq!(processed[0].label, "header");
        assert_eq!(processed[2].label, "inline_formula");
        assert_eq!(processed[3].label, "footer");
    }

    #[test]
    fn decode_uses_top_query_class_and_restores_page_coordinates() {
        let mut logits = vec![-20.0; 2 * LABELS.len()];
        logits[22] = 4.0;
        logits[LABELS.len() + 21] = 3.0;
        let boxes = vec![0.5, 0.5, 0.5, 0.5, 0.25, 0.25, 0.2, 0.2];
        let order = vec![0.0, 10.0, -10.0, 0.0];
        let bundle = TensorBundle {
            metadata: BTreeMap::new(),
            tensors: vec![
                Tensor::from_f32("logits", vec![1, 2, LABELS.len()], &logits),
                Tensor::from_f32("pred_boxes", vec![1, 2, 4], &boxes),
                Tensor::from_f32("order_logits", vec![1, 2, 2], &order),
            ],
        };

        let decoded = decode(&bundle, 1000, 500, 0.45).unwrap();

        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].label, "text");
        assert_eq!(decoded[0].bbox, [250.0, 125.0, 750.0, 375.0]);
        assert_eq!(decoded[1].label, "table");
        assert_eq!(decoded[1].bbox, [150.0, 75.0, 350.0, 175.0]);
    }
}
