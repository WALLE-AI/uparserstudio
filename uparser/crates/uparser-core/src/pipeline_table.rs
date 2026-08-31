//! Rust-owned table candidate scoring and wired/wireless selection.

use crate::tensor_wire::{Tensor, TensorBundle, TensorDType, TensorWireError};
use std::collections::{BTreeMap, HashSet};

const SLANET_TOKENS: [&str; 50] = [
    "sos",
    "<thead>",
    "</thead>",
    "<tbody>",
    "</tbody>",
    "<tr>",
    "</tr>",
    "<td",
    ">",
    "</td>",
    " colspan=\"2\"",
    " colspan=\"3\"",
    " colspan=\"4\"",
    " colspan=\"5\"",
    " colspan=\"6\"",
    " colspan=\"7\"",
    " colspan=\"8\"",
    " colspan=\"9\"",
    " colspan=\"10\"",
    " colspan=\"11\"",
    " colspan=\"12\"",
    " colspan=\"13\"",
    " colspan=\"14\"",
    " colspan=\"15\"",
    " colspan=\"16\"",
    " colspan=\"17\"",
    " colspan=\"18\"",
    " colspan=\"19\"",
    " colspan=\"20\"",
    " rowspan=\"2\"",
    " rowspan=\"3\"",
    " rowspan=\"4\"",
    " rowspan=\"5\"",
    " rowspan=\"6\"",
    " rowspan=\"7\"",
    " rowspan=\"8\"",
    " rowspan=\"9\"",
    " rowspan=\"10\"",
    " rowspan=\"11\"",
    " rowspan=\"12\"",
    " rowspan=\"13\"",
    " rowspan=\"14\"",
    " rowspan=\"15\"",
    " rowspan=\"16\"",
    " rowspan=\"17\"",
    " rowspan=\"18\"",
    " rowspan=\"19\"",
    " rowspan=\"20\"",
    "<td></td>",
    "eos",
];

#[derive(Debug, Clone, PartialEq)]
pub struct TableCellCandidate {
    pub text: String,
    pub row: u32,
    pub column: u32,
    pub row_span: u32,
    pub column_span: u32,
    pub bbox: Option<[f32; 8]>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TableCandidate {
    pub html: String,
    pub cells: Vec<TableCellCandidate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectedTableModel {
    Wired,
    Wireless,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TableClassification {
    pub model: SelectedTableModel,
    pub confidence: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableSelection {
    pub model: SelectedTableModel,
    pub reason: &'static str,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SlanetDecoded {
    pub tokens: Vec<String>,
    pub cells: Vec<TableCellCandidate>,
    pub confidence: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WiredDecoded {
    pub cells: Vec<TableCellCandidate>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TableTextSpan {
    pub id: String,
    pub polygon: Vec<[f32; 2]>,
    pub text: String,
}

#[derive(Debug, thiserror::Error)]
pub enum TableError {
    #[error(transparent)]
    Tensor(#[from] TensorWireError),
    #[error("table crop has zero width or height")]
    EmptyCrop,
    #[error("missing table tensor: {0}")]
    MissingTensor(&'static str),
    #[error("table tensor {name} has invalid shape {shape:?}")]
    InvalidShape {
        name: &'static str,
        shape: Vec<usize>,
    },
}

fn bilinear(image: &image::RgbImage, x: f32, y: f32, channel: usize) -> u8 {
    let base_x = x.floor() as i32;
    let base_y = y.floor() as i32;
    let max_x = image.width().saturating_sub(1) as i32;
    let max_y = image.height().saturating_sub(1) as i32;
    let x0 = base_x.clamp(0, max_x) as u32;
    let x1 = (base_x + 1).clamp(0, max_x) as u32;
    let y0 = base_y.clamp(0, max_y) as u32;
    let y1 = (base_y + 1).clamp(0, max_y) as u32;
    let wx = x - x.floor();
    let wy = y - y.floor();
    let top = f32::from(image.get_pixel(x0, y0)[channel]) * (1.0 - wx)
        + f32::from(image.get_pixel(x1, y0)[channel]) * wx;
    let bottom = f32::from(image.get_pixel(x0, y1)[channel]) * (1.0 - wx)
        + f32::from(image.get_pixel(x1, y1)[channel]) * wx;
    (top * (1.0 - wy) + bottom * wy).round().clamp(0.0, 255.0) as u8
}

pub fn preprocess_slanet(image: &image::RgbImage) -> Result<TensorBundle, TableError> {
    if image.width() == 0 || image.height() == 0 {
        return Err(TableError::EmptyCrop);
    }
    let ratio = 488.0 / image.width().max(image.height()) as f32;
    let resized_width = (image.width() as f32 * ratio) as usize;
    let resized_height = (image.height() as f32 * ratio) as usize;
    let plane = 488 * 488;
    let mut values = vec![0.0f32; plane * 3];
    let mean = [0.485f32, 0.456, 0.406];
    let std = [0.229f32, 0.224, 0.225];
    for y in 0..resized_height {
        let source_y = (y as f32 + 0.5) * image.height() as f32 / resized_height as f32 - 0.5;
        for x in 0..resized_width {
            let source_x = (x as f32 + 0.5) * image.width() as f32 / resized_width as f32 - 0.5;
            for channel in 0..3 {
                let pixel = bilinear(image, source_x, source_y, channel);
                values[channel * plane + y * 488 + x] =
                    (f32::from(pixel) / 255.0 - mean[channel]) / std[channel];
            }
        }
    }
    Ok(TensorBundle {
        metadata: BTreeMap::from([
            ("model".into(), "slanet_plus".into()),
            ("original_width".into(), image.width().to_string()),
            ("original_height".into(), image.height().to_string()),
            ("resize_ratio".into(), ratio.to_string()),
        ]),
        tensors: vec![Tensor::from_f32("x", vec![1, 3, 488, 488], &values)],
    })
}

pub fn preprocess_classifier(image: &image::RgbImage) -> Result<TensorBundle, TableError> {
    if image.width() == 0 || image.height() == 0 {
        return Err(TableError::EmptyCrop);
    }
    let ratio = 256.0 / image.width().min(image.height()) as f32;
    let resized_width = (image.width() as f32 * ratio).round_ties_even() as usize;
    let resized_height = (image.height() as f32 * ratio).round_ties_even() as usize;
    let start_x = resized_width.saturating_sub(224) / 2;
    let start_y = resized_height.saturating_sub(224) / 2;
    let plane = 224 * 224;
    let mut values = vec![0.0f32; plane * 3];
    let mean = [0.485f32, 0.456, 0.406];
    let std = [0.229f32, 0.224, 0.225];
    for y in 0..224 {
        let resized_y = y + start_y;
        let source_y =
            (resized_y as f32 + 0.5) * image.height() as f32 / resized_height as f32 - 0.5;
        for x in 0..224 {
            let resized_x = x + start_x;
            let source_x =
                (resized_x as f32 + 0.5) * image.width() as f32 / resized_width as f32 - 0.5;
            for channel in 0..3 {
                let pixel = bilinear(image, source_x, source_y, channel);
                values[channel * plane + y * 224 + x] =
                    (f32::from(pixel) / 255.0 - mean[channel]) / std[channel];
            }
        }
    }
    Ok(TensorBundle {
        metadata: BTreeMap::from([("model".into(), "paddle_table_cls".into())]),
        tensors: vec![Tensor::from_f32("x", vec![1, 3, 224, 224], &values)],
    })
}

fn area_resize_rgb(image: &image::RgbImage, width: usize, height: usize) -> Vec<[f32; 3]> {
    let source_width = image.width() as usize;
    let source_height = image.height() as usize;
    let horizontal_scale = source_width as f32 / width as f32;
    let vertical_scale = source_height as f32 / height as f32;
    let mut horizontal = vec![[0.0; 3]; source_height * width];
    for y in 0..source_height {
        for target_x in 0..width {
            let start = target_x as f32 * horizontal_scale;
            let end = (target_x + 1) as f32 * horizontal_scale;
            for source_x in start.floor() as usize..end.ceil() as usize {
                if source_x >= source_width {
                    continue;
                }
                let weight = (end.min(source_x as f32 + 1.0) - start.max(source_x as f32)).max(0.0);
                let pixel = image.get_pixel(source_x as u32, y as u32);
                for channel in 0..3 {
                    horizontal[y * width + target_x][channel] +=
                        f32::from(pixel[channel]) * weight / horizontal_scale;
                }
            }
        }
    }
    let mut output = vec![[0.0; 3]; height * width];
    for target_y in 0..height {
        let start = target_y as f32 * vertical_scale;
        let end = (target_y + 1) as f32 * vertical_scale;
        for source_y in start.floor() as usize..end.ceil() as usize {
            if source_y >= source_height {
                continue;
            }
            let weight = (end.min(source_y as f32 + 1.0) - start.max(source_y as f32)).max(0.0);
            for x in 0..width {
                for channel in 0..3 {
                    output[target_y * width + x][channel] +=
                        horizontal[source_y * width + x][channel] * weight / vertical_scale;
                }
            }
        }
    }
    output
}

pub fn preprocess_unet(image: &image::RgbImage) -> Result<TensorBundle, TableError> {
    if image.width() == 0 || image.height() == 0 {
        return Err(TableError::EmptyCrop);
    }
    let scale = (1024.0 / image.width().max(image.height()) as f32)
        .min(1024.0 / image.width().min(image.height()) as f32);
    let width = (image.width() as f32 * scale + 0.5) as usize;
    let height = (image.height() as f32 * scale + 0.5) as usize;
    let plane = width * height;
    let mut values = vec![0.0f32; plane * 3];
    let mean = [123.675f32, 116.28, 103.53];
    let std = [58.395f32, 57.12, 57.375];
    if image.width() > 1024 && image.height() > 1024 {
        let resized = area_resize_rgb(image, width, height);
        for y in 0..height {
            for x in 0..width {
                for channel in 0..3 {
                    values[channel * plane + y * width + x] =
                        (resized[y * width + x][channel] - mean[channel]) / std[channel];
                }
            }
        }
    } else {
        for y in 0..height {
            let source_y = (y as f32 + 0.5) * image.height() as f32 / height as f32 - 0.5;
            for x in 0..width {
                let source_x = (x as f32 + 0.5) * image.width() as f32 / width as f32 - 0.5;
                for channel in 0..3 {
                    values[channel * plane + y * width + x] =
                        (f32::from(bilinear(image, source_x, source_y, channel)) - mean[channel])
                            / std[channel];
                }
            }
        }
    }
    Ok(TensorBundle {
        metadata: BTreeMap::from([
            ("model".into(), "unet_table_structure".into()),
            ("original_width".into(), image.width().to_string()),
            ("original_height".into(), image.height().to_string()),
        ]),
        tensors: vec![Tensor::from_f32(
            "input",
            vec![1, 3, height, width],
            &values,
        )],
    })
}

#[derive(Debug, Clone, Copy)]
struct ComponentBox {
    left: usize,
    top: usize,
    right: usize,
    bottom: usize,
}

fn component_boxes(mask: &[bool], width: usize, height: usize) -> Vec<ComponentBox> {
    let mut seen = vec![false; mask.len()];
    let mut boxes = Vec::new();
    for seed in 0..mask.len() {
        if !mask[seed] || seen[seed] {
            continue;
        }
        seen[seed] = true;
        let mut stack = vec![seed];
        let mut bbox = ComponentBox {
            left: seed % width,
            right: seed % width,
            top: seed / width,
            bottom: seed / width,
        };
        while let Some(index) = stack.pop() {
            let x = index % width;
            let y = index / width;
            bbox.left = bbox.left.min(x);
            bbox.right = bbox.right.max(x);
            bbox.top = bbox.top.min(y);
            bbox.bottom = bbox.bottom.max(y);
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let nx = x as i32 + dx;
                    let ny = y as i32 + dy;
                    if nx < 0 || ny < 0 || nx >= width as i32 || ny >= height as i32 {
                        continue;
                    }
                    let neighbor = ny as usize * width + nx as usize;
                    if mask[neighbor] && !seen[neighbor] {
                        seen[neighbor] = true;
                        stack.push(neighbor);
                    }
                }
            }
        }
        boxes.push(bbox);
    }
    boxes
}

fn morph_close(
    mask: &[bool],
    width: usize,
    height: usize,
    kernel: usize,
    horizontal: bool,
) -> Vec<bool> {
    fn pass(
        mask: &[bool],
        width: usize,
        height: usize,
        radius: usize,
        horizontal: bool,
        dilate: bool,
    ) -> Vec<bool> {
        let mut output = vec![false; mask.len()];
        for y in 0..height {
            for x in 0..width {
                let (start, end) = if horizontal {
                    (x.saturating_sub(radius), (x + radius).min(width - 1))
                } else {
                    (y.saturating_sub(radius), (y + radius).min(height - 1))
                };
                output[y * width + x] = if dilate {
                    (start..=end).any(|position| {
                        let index = if horizontal {
                            y * width + position
                        } else {
                            position * width + x
                        };
                        mask[index]
                    })
                } else {
                    (start..=end).all(|position| {
                        let index = if horizontal {
                            y * width + position
                        } else {
                            position * width + x
                        };
                        mask[index]
                    })
                };
            }
        }
        output
    }
    let radius = kernel.saturating_sub(1) / 2;
    let dilated = pass(mask, width, height, radius, horizontal, true);
    pass(&dilated, width, height, radius, horizontal, false)
}

fn line_distance(left: [f32; 2], right: [f32; 2]) -> f32 {
    (left[0] - right[0]).hypot(left[1] - right[1])
}

fn adjust_lines(
    lines: &[[f32; 4]],
    distance_threshold: f32,
    angle_threshold: f32,
) -> Vec<[f32; 4]> {
    let mut additions = Vec::new();
    for (left_index, left) in lines.iter().enumerate() {
        let left_center = [(left[0] + left[2]) * 0.5, (left[1] + left[3]) * 0.5];
        for (right_index, right) in lines.iter().enumerate() {
            if left_index == right_index {
                continue;
            }
            let right_center = [(right[0] + right[2]) * 0.5, (right[1] + right[3]) * 0.5];
            if (right[0] < left_center[0] && left_center[0] < right[2])
                || (right[1] < left_center[1] && left_center[1] < right[3])
                || (left[0] < right_center[0] && right_center[0] < left[2])
                || (left[1] < right_center[1] && right_center[1] < left[3])
            {
                continue;
            }
            for start in [[left[0], left[1]], [left[2], left[3]]] {
                for end in [[right[0], right[1]], [right[2], right[3]]] {
                    let dx = end[0] - start[0];
                    let angle = ((end[1] - start[1]).abs() / (dx.abs() + 1e-10))
                        .atan()
                        .to_degrees();
                    if line_distance(start, end) < distance_threshold && angle < angle_threshold {
                        additions.push([start[0], start[1], end[0], end[1]]);
                    }
                }
            }
        }
    }
    additions
}

fn extend_line_to_crossing(line: [f32; 4], other: [f32; 4]) -> [f32; 4] {
    let [x1, y1, x2, y2] = line;
    let [ox1, oy1, ox2, oy2] = other;
    let a1 = y2 - y1;
    let b1 = x1 - x2;
    let c1 = x2 * y1 - x1 * y2;
    let a2 = oy2 - oy1;
    let b2 = ox1 - ox2;
    let c2 = ox2 * oy1 - ox1 * oy2;
    let side1 = a2 * x1 + b2 * y1 + c2;
    let side2 = a2 * x2 + b2 * y2 + c2;
    if side1 * side2 <= 0.0 {
        return line;
    }
    let denominator = a1 * b2 - a2 * b1;
    if denominator.abs() <= f32::EPSILON {
        return line;
    }
    let crossing = [
        (b1 * c2 - b2 * c1) / denominator,
        (a2 * c1 - a1 * c2) / denominator,
    ];
    let first_distance = line_distance(crossing, [x1, y1]);
    let second_distance = line_distance(crossing, [x2, y2]);
    if first_distance.min(second_distance) >= 20.0 {
        return line;
    }
    let anchor = if first_distance < second_distance {
        [x2, y2]
    } else {
        [x1, y1]
    };
    let angle = ((anchor[1] - crossing[1]).abs() / ((anchor[0] - crossing[0]).abs() + 1e-10))
        .atan()
        .to_degrees();
    if angle >= 30.0 && (90.0 - angle).abs() >= 30.0 {
        return line;
    }
    if first_distance < second_distance {
        [crossing[0], crossing[1], x2, y2]
    } else {
        [x1, y1, crossing[0], crossing[1]]
    }
}

fn recover_logical_cells(boxes: &[ComponentBox]) -> Vec<(u32, u32, u32, u32)> {
    if boxes.is_empty() {
        return Vec::new();
    }
    let mut rows = vec![vec![0usize]];
    for index in 1..boxes.len() {
        if (boxes[index].top as i32 - boxes[index - 1].top as i32).unsigned_abs() > 10 {
            rows.push(Vec::new());
        }
        rows.last_mut().unwrap().push(index);
    }
    let longest = rows.iter().max_by_key(|row| row.len()).unwrap();
    let mut column_starts = longest
        .iter()
        .map(|&index| boxes[index].left as f32)
        .collect::<Vec<_>>();
    let mut min_x = column_starts[0];
    let mut max_x = boxes[*longest.last().unwrap()].right as f32;
    for row in &rows {
        for &index in row {
            for (value, insert_last) in [
                (boxes[index].left as f32, true),
                (boxes[index].right as f32, false),
            ] {
                if column_starts
                    .iter()
                    .any(|existing| (value - existing).abs() <= 15.0)
                {
                    continue;
                }
                if value < min_x {
                    column_starts.insert(0, value);
                    min_x = value;
                } else if value > max_x {
                    if insert_last {
                        column_starts.push(value);
                    }
                    max_x = value;
                } else if let Some(position) = column_starts.iter().position(|&item| value < item) {
                    column_starts.insert(position, value);
                }
            }
        }
    }
    let mut column_widths = column_starts
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .collect::<Vec<_>>();
    column_widths.push(max_x - column_starts.last().copied().unwrap_or(max_x));
    let row_starts = rows
        .iter()
        .map(|row| boxes[row[0]].top as f32)
        .collect::<Vec<_>>();
    let mut row_heights = row_starts
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .collect::<Vec<_>>();
    row_heights.push(
        rows.last()
            .unwrap()
            .iter()
            .map(|&index| (boxes[index].bottom - boxes[index].top) as f32)
            .fold(0.0, f32::max),
    );

    let mut logical = vec![(0, 0, 1, 1); boxes.len()];
    for (row_index, row) in rows.iter().enumerate() {
        let mut prior_column_spans = 0usize;
        for &box_index in row {
            let box_width = (boxes[box_index].right - boxes[box_index].left) as f32;
            let nearest = column_starts
                .iter()
                .enumerate()
                .min_by(|left, right| {
                    (left.1 - boxes[box_index].left as f32)
                        .abs()
                        .total_cmp(&(right.1 - boxes[box_index].left as f32).abs())
                })
                .map_or(0, |(index, _)| index);
            let column = prior_column_spans.max(nearest);
            let column_span = closest_span(&column_widths, column, box_width);
            prior_column_spans += column_span;
            let box_height = (boxes[box_index].bottom - boxes[box_index].top) as f32;
            let row_span = closest_span(&row_heights, row_index, box_height);
            logical[box_index] = (
                row_index as u32,
                column as u32,
                row_span as u32,
                column_span as u32,
            );
        }
    }
    logical
}

fn closest_span(axis_sizes: &[f32], start: usize, target: f32) -> usize {
    let mut cumulative = 0.0;
    let mut previous_difference = f32::INFINITY;
    for (offset, &size) in axis_sizes.iter().skip(start).enumerate() {
        cumulative += size;
        let difference = (cumulative - target).abs();
        if offset == 0 && cumulative > target {
            return 1;
        }
        if difference <= 10.0 {
            return offset + 1;
        }
        if cumulative > target {
            return if difference < previous_difference {
                offset + 1
            } else {
                offset.max(1)
            };
        }
        previous_difference = difference;
    }
    axis_sizes.len().saturating_sub(start).max(1)
}

fn cluster_coordinates(mut values: Vec<f32>, threshold: f32) -> Vec<f32> {
    values.sort_by(f32::total_cmp);
    let mut clusters: Vec<(f32, usize)> = Vec::new();
    for value in values {
        if let Some((mean, count)) = clusters.last_mut()
            && (value - *mean).abs() <= threshold
        {
            *mean = (*mean * *count as f32 + value) / (*count + 1) as f32;
            *count += 1;
            continue;
        }
        clusters.push((value, 1));
    }
    clusters.into_iter().map(|(mean, _)| mean).collect()
}

fn nearest_boundary(boundaries: &[f32], value: f32) -> usize {
    boundaries
        .iter()
        .enumerate()
        .min_by(|left, right| (left.1 - value).abs().total_cmp(&(right.1 - value).abs()))
        .map_or(0, |(index, _)| index)
}

pub fn decode_unet(
    bundle: &TensorBundle,
    original_size: (u32, u32),
) -> Result<WiredDecoded, TableError> {
    let tensor = bundle
        .tensors
        .iter()
        .find(|tensor| tensor.name == "segmentation")
        .ok_or(TableError::MissingTensor("segmentation"))?;
    if tensor.dtype != TensorDType::I64
        || tensor.shape.len() != 4
        || tensor.shape[0] != 1
        || tensor.shape[1] != 1
    {
        return Err(TableError::InvalidShape {
            name: "segmentation",
            shape: tensor.shape.clone(),
        });
    }
    let source_height = tensor.shape[2];
    let source_width = tensor.shape[3];
    let labels: Vec<i64> = tensor
        .data
        .chunks_exact(8)
        .map(|bytes| i64::from_le_bytes(bytes.try_into().expect("eight-byte chunk")))
        .collect();
    let width = original_size.0 as usize;
    let height = original_size.1 as usize;
    let mut horizontal = vec![false; width * height];
    let mut vertical = vec![false; width * height];
    for y in 0..height {
        let source_y = (y * source_height / height).min(source_height - 1);
        for x in 0..width {
            let source_x = (x * source_width / width).min(source_width - 1);
            match labels[source_y * source_width + source_x] {
                1 => horizontal[y * width + x] = true,
                2 => vertical[y * width + x] = true,
                _ => {}
            }
        }
    }
    let horizontal = morph_close(
        &horizontal,
        width,
        height,
        ((source_width as f32).sqrt() * 1.2) as usize,
        true,
    );
    let vertical = morph_close(
        &vertical,
        width,
        height,
        ((source_height as f32).sqrt() * 1.2) as usize,
        false,
    );
    let enhanced_recovery = width > 1024 && height > 1024;
    let mut rows: Vec<[f32; 4]> = component_boxes(&horizontal, width, height)
        .into_iter()
        .filter(|bbox| bbox.right - bbox.left + 1 > 50)
        .map(|bbox| {
            let y = (bbox.top + bbox.bottom) as f32 * 0.5;
            [bbox.left as f32, y, bbox.right as f32, y]
        })
        .collect();
    let mut columns: Vec<[f32; 4]> = component_boxes(&vertical, width, height)
        .into_iter()
        .filter(|bbox| bbox.bottom - bbox.top + 1 > 30)
        .map(|bbox| {
            let x = (bbox.left + bbox.right) as f32 * 0.5;
            [x, bbox.top as f32, x, bbox.bottom as f32]
        })
        .collect();
    if enhanced_recovery {
        rows.extend(adjust_lines(&rows, 100.0, 50.0));
        columns.extend(adjust_lines(&columns, 15.0, 50.0));
        for row in &mut rows {
            for column in &mut columns {
                *row = extend_line_to_crossing(*row, *column);
                *column = extend_line_to_crossing(*column, *row);
            }
        }
    }
    let mut line_mask = vec![false; width * height];
    for row in &rows {
        let y = row[1].round().clamp(0.0, height.saturating_sub(1) as f32) as usize;
        let left = row[0].floor().max(0.0) as usize;
        let right = row[2].ceil().min(width.saturating_sub(1) as f32) as usize;
        for yy in y.saturating_sub(1)..=(y + 1).min(height - 1) {
            for x in left..=right {
                line_mask[yy * width + x] = true;
            }
        }
    }
    for column in &columns {
        let x = column[0].round().clamp(0.0, width.saturating_sub(1) as f32) as usize;
        let top = column[1].floor().max(0.0) as usize;
        let bottom = column[3].ceil().min(height.saturating_sub(1) as f32) as usize;
        for y in top..=bottom {
            for xx in x.saturating_sub(1)..=(x + 1).min(width - 1) {
                line_mask[y * width + xx] = true;
            }
        }
    }
    let inverse: Vec<bool> = line_mask.iter().map(|value| !value).collect();
    let mut physical: Vec<ComponentBox> = component_boxes(&inverse, width, height)
        .into_iter()
        .filter(|bbox| {
            let box_width = bbox.right - bbox.left + 1;
            let box_height = bbox.bottom - bbox.top + 1;
            let box_area = box_width * box_height;
            box_area <= width * height * 3 / 4
                && box_area < width * height / 2
                && box_width >= 15
                && box_height >= 15
        })
        .collect();
    physical.sort_by_key(|bbox| (bbox.top, bbox.left));
    let logical = if enhanced_recovery {
        recover_logical_cells(&physical)
    } else {
        let x_boundaries = cluster_coordinates(
            physical
                .iter()
                .flat_map(|bbox| [bbox.left as f32, (bbox.right + 1) as f32])
                .collect(),
            15.0,
        );
        let y_boundaries = cluster_coordinates(
            physical
                .iter()
                .flat_map(|bbox| [bbox.top as f32, (bbox.bottom + 1) as f32])
                .collect(),
            10.0,
        );
        physical
            .iter()
            .map(|bbox| {
                let column = nearest_boundary(&x_boundaries, bbox.left as f32);
                let column_end = nearest_boundary(&x_boundaries, (bbox.right + 1) as f32);
                let row = nearest_boundary(&y_boundaries, bbox.top as f32);
                let row_end = nearest_boundary(&y_boundaries, (bbox.bottom + 1) as f32);
                (
                    row as u32,
                    column as u32,
                    row_end.saturating_sub(row).max(1) as u32,
                    column_end.saturating_sub(column).max(1) as u32,
                )
            })
            .collect()
    };
    let mut cells: Vec<TableCellCandidate> = physical
        .into_iter()
        .zip(logical)
        .map(
            |(bbox, (row, column, row_span, column_span))| TableCellCandidate {
                text: String::new(),
                row,
                column,
                row_span,
                column_span,
                bbox: Some([
                    bbox.left as f32,
                    bbox.top as f32,
                    bbox.right as f32,
                    bbox.top as f32,
                    bbox.right as f32,
                    bbox.bottom as f32,
                    bbox.left as f32,
                    bbox.bottom as f32,
                ]),
            },
        )
        .collect();
    cells.sort_by_key(|cell| (cell.row, cell.column));
    Ok(WiredDecoded { cells })
}

pub fn decode_classifier(bundle: &TensorBundle) -> Result<TableClassification, TableError> {
    let (shape, values) = f32_tensor(bundle, "logits")?;
    if shape != [1, 2] {
        return Err(TableError::InvalidShape {
            name: "logits",
            shape: shape.to_vec(),
        });
    }
    let (index, &confidence) = values
        .iter()
        .enumerate()
        .max_by(|left, right| left.1.total_cmp(right.1))
        .expect("fixed table classifier output");
    Ok(TableClassification {
        model: if index == 0 {
            SelectedTableModel::Wired
        } else {
            SelectedTableModel::Wireless
        },
        confidence,
    })
}

fn f32_tensor<'a>(
    bundle: &'a TensorBundle,
    name: &'static str,
) -> Result<(&'a [usize], Vec<f32>), TableError> {
    let tensor = bundle
        .tensors
        .iter()
        .find(|tensor| tensor.name == name)
        .ok_or(TableError::MissingTensor(name))?;
    if tensor.dtype != TensorDType::F32 {
        return Err(TableError::Tensor(TensorWireError::UnexpectedDType {
            name: name.into(),
            expected: TensorDType::F32,
            actual: tensor.dtype,
        }));
    }
    Ok((&tensor.shape, tensor.to_f32()?))
}

fn attribute_span(token: &str, name: &str) -> Option<u32> {
    token
        .strip_prefix(&format!(" {name}=\""))?
        .strip_suffix('"')?
        .parse()
        .ok()
}

fn logical_cells(tokens: &[String]) -> Vec<(u32, u32, u32, u32)> {
    let mut result = Vec::new();
    let mut occupied = HashSet::new();
    let mut row = 0u32;
    let mut column = 0u32;
    let mut index = 0usize;
    while index < tokens.len() {
        match tokens[index].as_str() {
            "<tr>" => column = 0,
            "</tr>" => row += 1,
            token if token.starts_with("<td") => {
                let mut row_span = 1;
                let mut column_span = 1;
                if token != "<td></td>" {
                    index += 1;
                    while index < tokens.len() && !tokens[index].starts_with('>') {
                        row_span = attribute_span(&tokens[index], "rowspan").unwrap_or(row_span);
                        column_span =
                            attribute_span(&tokens[index], "colspan").unwrap_or(column_span);
                        index += 1;
                    }
                }
                while occupied.contains(&(row, column)) {
                    column += 1;
                }
                result.push((row, column, row_span, column_span));
                for occupied_row in row..row + row_span {
                    for occupied_column in column..column + column_span {
                        occupied.insert((occupied_row, occupied_column));
                    }
                }
                column += column_span;
            }
            _ => {}
        }
        index += 1;
    }
    result
}

pub fn decode_slanet(
    bundle: &TensorBundle,
    original_size: (u32, u32),
) -> Result<SlanetDecoded, TableError> {
    let (location_shape, locations) = f32_tensor(bundle, "loc_preds")?;
    let (structure_shape, structures) = f32_tensor(bundle, "structure_probs")?;
    if structure_shape.len() != 3 || structure_shape[0] != 1 || structure_shape[2] != 50 {
        return Err(TableError::InvalidShape {
            name: "structure_probs",
            shape: structure_shape.to_vec(),
        });
    }
    let steps = structure_shape[1];
    if location_shape != [1, steps, 8] {
        return Err(TableError::InvalidShape {
            name: "loc_preds",
            shape: location_shape.to_vec(),
        });
    }
    let ratio = 488.0 / original_size.0.max(original_size.1) as f32;
    let width_compensation = 488.0 / (original_size.0 as f32 * ratio);
    let height_compensation = 488.0 / (original_size.1 as f32 * ratio);
    let mut tokens = Vec::new();
    let mut boxes = Vec::new();
    let mut scores = Vec::new();
    for step in 0..steps {
        let row = &structures[step * 50..step * 50 + 50];
        let (class, &score) = row
            .iter()
            .enumerate()
            .max_by(|left, right| left.1.total_cmp(right.1))
            .expect("fixed non-empty SLANet vocabulary");
        if step > 0 && class == 49 {
            break;
        }
        if matches!(class, 0 | 49) {
            continue;
        }
        let token = SLANET_TOKENS[class];
        if matches!(token, "<td" | "<td></td>") {
            let mut bbox = [0.0f32; 8];
            for coordinate in 0..8 {
                let scale = if coordinate % 2 == 0 {
                    original_size.0 as f32 * width_compensation
                } else {
                    original_size.1 as f32 * height_compensation
                };
                bbox[coordinate] = locations[step * 8 + coordinate] * scale;
            }
            boxes.push(bbox);
        }
        tokens.push(token.to_owned());
        scores.push(score);
    }
    let logical = logical_cells(&tokens);
    let cells = logical
        .into_iter()
        .enumerate()
        .map(
            |(index, (row, column, row_span, column_span))| TableCellCandidate {
                text: String::new(),
                row,
                column,
                row_span,
                column_span,
                bbox: boxes.get(index).copied(),
            },
        )
        .collect();
    let confidence = if scores.is_empty() {
        f32::NAN
    } else {
        scores.iter().sum::<f32>() / scores.len() as f32
    };
    Ok(SlanetDecoded {
        tokens,
        cells,
        confidence,
    })
}

fn signed_polygon_area(points: &[[f32; 2]]) -> f32 {
    if points.len() < 3 {
        return 0.0;
    }
    points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .take(points.len())
        .map(|(left, right)| left[0] * right[1] - right[0] * left[1])
        .sum::<f32>()
        * 0.5
}

fn line_intersection(
    start: [f32; 2],
    end: [f32; 2],
    clip_start: [f32; 2],
    clip_end: [f32; 2],
) -> [f32; 2] {
    let segment = [end[0] - start[0], end[1] - start[1]];
    let clip = [clip_end[0] - clip_start[0], clip_end[1] - clip_start[1]];
    let denominator = segment[0] * clip[1] - segment[1] * clip[0];
    if denominator.abs() <= f32::EPSILON {
        return end;
    }
    let offset = [clip_start[0] - start[0], clip_start[1] - start[1]];
    let along = (offset[0] * clip[1] - offset[1] * clip[0]) / denominator;
    [start[0] + segment[0] * along, start[1] + segment[1] * along]
}

fn polygon_intersection_area(subject: &[[f32; 2]], clip: &[[f32; 2]]) -> f32 {
    if subject.len() < 3 || clip.len() < 3 {
        return 0.0;
    }
    let mut clip_points = clip.to_vec();
    if signed_polygon_area(&clip_points) < 0.0 {
        clip_points.reverse();
    }
    let mut output = subject.to_vec();
    for edge in 0..clip_points.len() {
        let clip_start = clip_points[edge];
        let clip_end = clip_points[(edge + 1) % clip_points.len()];
        let input = std::mem::take(&mut output);
        if input.is_empty() {
            break;
        }
        let inside = |point: [f32; 2]| {
            (clip_end[0] - clip_start[0]) * (point[1] - clip_start[1])
                - (clip_end[1] - clip_start[1]) * (point[0] - clip_start[0])
                >= -1e-4
        };
        let mut previous = *input.last().unwrap();
        for &current in &input {
            let current_inside = inside(current);
            let previous_inside = inside(previous);
            if current_inside {
                if !previous_inside {
                    output.push(line_intersection(previous, current, clip_start, clip_end));
                }
                output.push(current);
            } else if previous_inside {
                output.push(line_intersection(previous, current, clip_start, clip_end));
            }
            previous = current;
        }
    }
    signed_polygon_area(&output).abs()
}

fn cell_polygon(cell: &TableCellCandidate) -> Option<[[f32; 2]; 4]> {
    let bbox = cell.bbox?;
    Some([
        [bbox[0], bbox[1]],
        [bbox[2], bbox[3]],
        [bbox[4], bbox[5]],
        [bbox[6], bbox[7]],
    ])
}

/// Bind every OCR span to at most one cell, selected by maximum polygon
/// intersection area. Returns span IDs that were consumed by the table.
pub fn bind_spans_to_cells(
    cells: &mut [TableCellCandidate],
    spans: &[TableTextSpan],
) -> HashSet<String> {
    let mut order: Vec<usize> = (0..spans.len()).collect();
    order.sort_by(|left, right| {
        let anchor = |span: &TableTextSpan| {
            span.polygon
                .iter()
                .fold([f32::INFINITY, f32::INFINITY], |result, point| {
                    [result[0].min(point[0]), result[1].min(point[1])]
                })
        };
        let left_anchor = anchor(&spans[*left]);
        let right_anchor = anchor(&spans[*right]);
        left_anchor[1]
            .total_cmp(&right_anchor[1])
            .then_with(|| left_anchor[0].total_cmp(&right_anchor[0]))
    });
    let mut consumed = HashSet::new();
    for span_index in order {
        let span = &spans[span_index];
        let target = cells
            .iter()
            .enumerate()
            .filter_map(|(index, cell)| {
                let polygon = cell_polygon(cell)?;
                let overlap = polygon_intersection_area(&span.polygon, &polygon);
                (overlap > 0.0).then_some((index, overlap))
            })
            .max_by(|left, right| left.1.total_cmp(&right.1))
            .map(|(index, _)| index);
        if let Some(index) = target {
            if !cells[index].text.is_empty() {
                cells[index].text.push('\n');
            }
            cells[index].text.push_str(&span.text);
            consumed.insert(span.id.clone());
        }
    }
    consumed
}

fn escape_html(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

pub fn render_slanet_html(tokens: &[String], cells: &[TableCellCandidate]) -> String {
    let mut html = String::from("<html><body><table>");
    let mut cell_index = 0usize;
    for token in tokens {
        if token == "<td></td>" {
            html.push_str("<td>");
            if let Some(cell) = cells.get(cell_index) {
                html.push_str(&escape_html(&cell.text));
            }
            html.push_str("</td>");
            cell_index += 1;
        } else {
            html.push_str(token);
            if token == ">" {
                if let Some(cell) = cells.get(cell_index) {
                    html.push_str(&escape_html(&cell.text));
                }
                cell_index += 1;
            }
        }
    }
    html.push_str("</table></body></html>");
    html
}

pub fn render_wired_html(cells: &[TableCellCandidate]) -> String {
    let row_count = cells
        .iter()
        .map(|cell| cell.row + cell.row_span)
        .max()
        .unwrap_or(0);
    let column_count = cells
        .iter()
        .map(|cell| cell.column + cell.column_span)
        .max()
        .unwrap_or(0);
    let table_width = cells
        .iter()
        .filter_map(|cell| cell.bbox.map(|bbox| bbox[4]))
        .fold(0.0f32, f32::max);
    let table_height = cells
        .iter()
        .filter_map(|cell| cell.bbox.map(|bbox| bbox[5]))
        .fold(0.0f32, f32::max);
    let (row_start, row_end, column_start, column_end) =
        if table_width > 1024.0 && table_height > 1024.0 {
            trim_wired_bounds(cells, row_count, column_count)
        } else {
            (0, row_count, 0, column_count)
        };
    let mut html = String::from("<html><body><table><tbody>");
    for row in row_start..row_end {
        html.push_str("<tr>");
        for column in column_start..column_end {
            let Some(cell) = cells.iter().rev().find(|cell| {
                cell.row <= row
                    && row < cell.row + cell.row_span
                    && cell.column <= column
                    && column < cell.column + cell.column_span
            }) else {
                html.push_str("<td></td>");
                continue;
            };
            let clipped_row = cell.row.max(row_start);
            let clipped_column = cell.column.max(column_start);
            if row != clipped_row || column != clipped_column {
                continue;
            }
            let row_span = (cell.row + cell.row_span).min(row_end) - clipped_row;
            let column_span = (cell.column + cell.column_span).min(column_end) - clipped_column;
            html.push_str("<td");
            if row_span > 1 {
                html.push_str(&format!(" rowspan=\"{row_span}\""));
            }
            if column_span > 1 {
                html.push_str(&format!(" colspan=\"{column_span}\""));
            }
            html.push('>');
            html.push_str(&escape_html(&cell.text));
            html.push_str("</td>");
        }
        html.push_str("</tr>");
    }
    html.push_str("</tbody></table></body></html>");
    html
}

fn trim_wired_bounds(
    cells: &[TableCellCandidate],
    row_count: u32,
    column_count: u32,
) -> (u32, u32, u32, u32) {
    let axis_sizes = |rows: bool, count: u32| {
        (0..count)
            .map(|axis| {
                let mut sizes = cells
                    .iter()
                    .filter_map(|cell| {
                        let (start, span, size) = if rows {
                            (
                                cell.row,
                                cell.row_span,
                                cell.bbox.map(|bbox| bbox[5] - bbox[1]),
                            )
                        } else {
                            (
                                cell.column,
                                cell.column_span,
                                cell.bbox.map(|bbox| bbox[4] - bbox[0]),
                            )
                        };
                        (start <= axis && axis < start + span).then_some(size? / span.max(1) as f32)
                    })
                    .filter(|size| *size > 0.0)
                    .collect::<Vec<_>>();
                median(&mut sizes)
            })
            .collect::<Vec<_>>()
    };
    let row_sizes = axis_sizes(true, row_count);
    let column_sizes = axis_sizes(false, column_count);
    let mut row_start = 0;
    let mut row_end = row_count;
    let mut column_start = 0;
    let mut column_end = column_count;
    while row_start < row_end
        && wired_edge_is_noise(
            cells,
            true,
            row_start,
            row_start,
            row_end,
            column_start,
            column_end,
            &row_sizes,
        )
    {
        row_start += 1;
    }
    while row_start < row_end
        && wired_edge_is_noise(
            cells,
            true,
            row_end - 1,
            row_start,
            row_end,
            column_start,
            column_end,
            &row_sizes,
        )
    {
        row_end -= 1;
    }
    while column_start < column_end
        && wired_edge_is_noise(
            cells,
            false,
            column_start,
            row_start,
            row_end,
            column_start,
            column_end,
            &column_sizes,
        )
    {
        column_start += 1;
    }
    while column_start < column_end
        && wired_edge_is_noise(
            cells,
            false,
            column_end - 1,
            row_start,
            row_end,
            column_start,
            column_end,
            &column_sizes,
        )
    {
        column_end -= 1;
    }
    (row_start, row_end, column_start, column_end)
}

fn median(values: &mut [f32]) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f32::total_cmp);
    let middle = values.len() / 2;
    Some(if values.len() % 2 == 0 {
        (values[middle - 1] + values[middle]) * 0.5
    } else {
        values[middle]
    })
}

#[allow(clippy::too_many_arguments)]
fn wired_edge_is_noise(
    cells: &[TableCellCandidate],
    rows: bool,
    axis: u32,
    row_start: u32,
    row_end: u32,
    column_start: u32,
    column_end: u32,
    axis_sizes: &[Option<f32>],
) -> bool {
    let covers = |cell: &TableCellCandidate, row: u32, column: u32| {
        cell.row <= row
            && row < cell.row + cell.row_span
            && cell.column <= column
            && column < cell.column + cell.column_span
    };
    let positions = if rows {
        (column_start..column_end)
            .map(|column| (axis, column))
            .collect::<Vec<_>>()
    } else {
        (row_start..row_end)
            .map(|row| (row, axis))
            .collect::<Vec<_>>()
    };
    if positions.iter().any(|&(row, column)| {
        cells
            .iter()
            .rev()
            .find(|cell| covers(cell, row, column))
            .is_some_and(|cell| !cell.text.trim().is_empty())
    }) {
        return false;
    }
    let covered = positions
        .iter()
        .filter(|&&(row, column)| cells.iter().any(|cell| covers(cell, row, column)))
        .count();
    if covered == 0 || covered < positions.len() {
        return true;
    }
    let Some(size) = axis_sizes.get(axis as usize).copied().flatten() else {
        return false;
    };
    let mut references = axis_sizes
        .iter()
        .enumerate()
        .filter_map(|(index, value)| (index != axis as usize).then_some((*value)?))
        .filter(|value| *value > 0.0)
        .collect::<Vec<_>>();
    let Some(reference) = median(&mut references) else {
        return false;
    };
    let ratio = size / reference;
    !(0.35..=2.5).contains(&ratio)
}

fn matched_text_count(candidate: &TableCandidate, ocr_texts: &[String]) -> usize {
    ocr_texts
        .iter()
        .filter(|text| !text.is_empty() && candidate.html.contains(text.as_str()))
        .count()
}

/// Reproduce MinerU 3.4.5's wired-to-wireless fallback rules. The caller
/// supplies decoded cells, so selection does not need a Python HTML parser.
pub fn select_candidate(
    wired: &TableCandidate,
    wireless: &TableCandidate,
    ocr_texts: &[String],
) -> TableSelection {
    let wired_len = wired.cells.len();
    let wireless_len = wireless.cells.len();
    let wired_non_blank = wired
        .cells
        .iter()
        .filter(|cell| !cell.text.trim().is_empty())
        .count();
    let wireless_non_blank = wireless
        .cells
        .iter()
        .filter(|cell| !cell.text.trim().is_empty())
        .count();

    let mut structure_switch = false;
    if wireless_non_blank > wired_non_blank {
        let wired_scale = (wired_non_blank as f64).sqrt().round() as usize;
        let plus_two_columns = wired_non_blank + wired_scale * 2;
        let plus_two_rows = wired_scale * (wired_scale + 2);
        structure_switch = wireless_non_blank + 3 >= plus_two_columns.max(plus_two_rows);
    }
    if structure_switch {
        return TableSelection {
            model: SelectedTableModel::Wireless,
            reason: "wireless_has_materially_more_non_blank_cells",
        };
    }

    let gap = wireless_len as isize - wired_len as isize;
    if (0..=5).contains(&gap) && wired_len <= (wireless_len as f64 * 0.75).round() as usize {
        return TableSelection {
            model: SelectedTableModel::Wireless,
            reason: "wired_structure_is_too_sparse",
        };
    }
    if gap == 0 && wired_len <= 4 {
        return TableSelection {
            model: SelectedTableModel::Wireless,
            reason: "small_equal_structure_prefers_wireless",
        };
    }

    let wired_text = matched_text_count(wired, ocr_texts);
    let wireless_text = matched_text_count(wireless, ocr_texts);
    if wireless_text >= 10 && wired_text as f64 <= wireless_text as f64 * 0.6 {
        return TableSelection {
            model: SelectedTableModel::Wireless,
            reason: "wireless_preserves_materially_more_ocr_text",
        };
    }
    TableSelection {
        model: SelectedTableModel::Wired,
        reason: "wired_candidate_retained",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(non_blank: usize, blank: usize, html: &str) -> TableCandidate {
        let cells = (0..non_blank + blank)
            .map(|index| TableCellCandidate {
                text: if index < non_blank {
                    format!("cell-{index}")
                } else {
                    String::new()
                },
                row: index as u32,
                column: 0,
                row_span: 1,
                column_span: 1,
                bbox: None,
            })
            .collect();
        TableCandidate {
            html: html.into(),
            cells,
        }
    }

    #[test]
    fn wired_render_trims_only_abnormal_empty_edge_rows() {
        let cell = |row: u32, top: f32, bottom: f32, text: &str| TableCellCandidate {
            text: text.into(),
            row,
            column: 0,
            row_span: 1,
            column_span: 1,
            bbox: Some([0.0, top, 2000.0, top, 2000.0, bottom, 0.0, bottom]),
        };
        let html = render_wired_html(&[
            cell(0, 0.0, 20.0, ""),
            cell(1, 20.0, 1120.0, "header"),
            cell(2, 1120.0, 2220.0, "value"),
        ]);
        assert!(!html.contains("<tr><td></td></tr>"));
        assert_eq!(html.matches("<tr>").count(), 2);

        let html = render_wired_html(&[
            cell(0, 0.0, 1100.0, ""),
            cell(1, 1100.0, 2200.0, "header"),
            cell(2, 2200.0, 3300.0, "value"),
        ]);
        assert_eq!(html.matches("<tr>").count(), 3);

        let html = render_wired_html(&[
            TableCellCandidate {
                bbox: Some([0.0, 0.0, 100.0, 0.0, 100.0, 2.0, 0.0, 2.0]),
                ..cell(0, 0.0, 2.0, "")
            },
            TableCellCandidate {
                bbox: Some([0.0, 2.0, 100.0, 2.0, 100.0, 42.0, 0.0, 42.0]),
                ..cell(1, 2.0, 42.0, "header")
            },
        ]);
        assert_eq!(html.matches("<tr>").count(), 2);
    }

    #[test]
    fn materially_richer_wireless_structure_wins() {
        let selection =
            select_candidate(&candidate(4, 0, "wired"), &candidate(9, 0, "wireless"), &[]);
        assert_eq!(selection.model, SelectedTableModel::Wireless);
        assert_eq!(
            selection.reason,
            "wireless_has_materially_more_non_blank_cells"
        );
    }

    #[test]
    fn ocr_coverage_can_select_wireless() {
        let texts: Vec<String> = (0..10).map(|index| format!("t{index}")).collect();
        let wireless_html = texts.join(" ");
        let selection = select_candidate(
            &candidate(20, 0, "t0 t1 t2 t3 t4 t5"),
            &candidate(20, 0, &wireless_html),
            &texts,
        );
        assert_eq!(selection.model, SelectedTableModel::Wireless);
        assert_eq!(
            selection.reason,
            "wireless_preserves_materially_more_ocr_text"
        );
    }

    #[test]
    fn wired_remains_default_when_fallback_rules_do_not_fire() {
        let selection = select_candidate(&candidate(12, 1, "same"), &candidate(10, 3, "same"), &[]);
        assert_eq!(selection.model, SelectedTableModel::Wired);
    }

    #[test]
    fn slanet_decode_restores_structure_spans_and_cell_geometry() {
        let classes = [0usize, 5, 7, 10, 8, 9, 6, 49];
        let mut structure = vec![0.0f32; classes.len() * 50];
        for (step, class) in classes.into_iter().enumerate() {
            structure[step * 50 + class] = 1.0;
        }
        let mut locations = vec![0.0f32; classes.len() * 8];
        locations[2 * 8..3 * 8].copy_from_slice(&[0.1, 0.2, 0.5, 0.2, 0.5, 0.4, 0.1, 0.4]);
        let bundle = TensorBundle {
            metadata: BTreeMap::new(),
            tensors: vec![
                Tensor::from_f32("loc_preds", vec![1, classes.len(), 8], &locations),
                Tensor::from_f32("structure_probs", vec![1, classes.len(), 50], &structure),
            ],
        };
        let decoded = decode_slanet(&bundle, (200, 100)).unwrap();
        assert_eq!(decoded.cells.len(), 1);
        assert_eq!(decoded.cells[0].column_span, 2);
        assert_eq!(decoded.cells[0].bbox.unwrap()[0], 20.0);
        assert_eq!(decoded.cells[0].bbox.unwrap()[1], 40.0);
    }

    #[test]
    fn slanet_preprocess_resizes_to_488_and_zero_pads_normalized_image() {
        let image = image::RgbImage::from_pixel(200, 100, image::Rgb([255, 0, 0]));
        let bundle = preprocess_slanet(&image).unwrap();
        let tensor = &bundle.tensors[0];
        assert_eq!(tensor.shape, [1, 3, 488, 488]);
        let values = tensor.to_f32().unwrap();
        assert!(values[0] > 2.0);
        assert!(values[2 * 488 * 488] < -1.5);
        assert_eq!(values[487 * 488], 0.0);
    }

    #[test]
    fn classifier_preprocess_and_decode_are_rust_owned() {
        let image = image::RgbImage::from_pixel(400, 200, image::Rgb([255, 255, 255]));
        let input = preprocess_classifier(&image).unwrap();
        assert_eq!(input.tensors[0].shape, [1, 3, 224, 224]);
        let output = TensorBundle {
            metadata: BTreeMap::new(),
            tensors: vec![Tensor::from_f32("logits", vec![1, 2], &[0.2, 0.8])],
        };
        let classification = decode_classifier(&output).unwrap();
        assert_eq!(classification.model, SelectedTableModel::Wireless);
        assert_eq!(classification.confidence, 0.8);
    }

    #[test]
    fn polygon_binding_consumes_each_span_once_and_html_is_escaped() {
        let mut cells = vec![
            TableCellCandidate {
                text: String::new(),
                row: 0,
                column: 0,
                row_span: 1,
                column_span: 1,
                bbox: Some([0.0, 0.0, 60.0, 0.0, 60.0, 40.0, 0.0, 40.0]),
            },
            TableCellCandidate {
                text: String::new(),
                row: 0,
                column: 1,
                row_span: 1,
                column_span: 1,
                bbox: Some([40.0, 0.0, 100.0, 0.0, 100.0, 40.0, 40.0, 40.0]),
            },
        ];
        let spans = vec![TableTextSpan {
            id: "span-1".into(),
            polygon: vec![[45.0, 5.0], [95.0, 5.0], [95.0, 35.0], [45.0, 35.0]],
            text: "A&B".into(),
        }];
        let consumed = bind_spans_to_cells(&mut cells, &spans);
        assert_eq!(consumed, HashSet::from(["span-1".to_owned()]));
        assert!(cells[0].text.is_empty());
        assert_eq!(cells[1].text, "A&B");

        let html = render_slanet_html(
            &[
                "<tr>".into(),
                "<td></td>".into(),
                "<td></td>".into(),
                "</tr>".into(),
            ],
            &cells,
        );
        assert!(html.contains("<td>A&amp;B</td>"));
        assert_eq!(html.matches("A&amp;B").count(), 1);
    }

    #[test]
    fn unet_preprocess_is_rust_owned() {
        let image = image::RgbImage::from_pixel(400, 200, image::Rgb([255, 255, 255]));
        let input = preprocess_unet(&image).unwrap();
        assert_eq!(input.tensors[0].shape, [1, 3, 512, 1024]);
        assert_eq!(input.tensors[0].name, "input");
    }

    #[test]
    fn unet_area_resize_averages_downsampled_pixels() {
        let image = image::RgbImage::from_fn(2, 2, |x, y| match (x, y) {
            (0, 0) => image::Rgb([0, 0, 0]),
            (1, 0) => image::Rgb([100, 20, 0]),
            (0, 1) => image::Rgb([0, 60, 200]),
            _ => image::Rgb([100, 120, 200]),
        });
        let resized = area_resize_rgb(&image, 1, 1);
        assert_eq!(resized, [[50.0, 50.0, 100.0]]);
    }

    #[test]
    fn unet_grid_decode_recovers_cells_and_html() {
        let width = 100usize;
        let height = 100usize;
        let mut labels = vec![0i64; width * height];
        for &y in &[10usize, 50, 90] {
            for yy in y - 1..=y + 1 {
                for x in 10..=90 {
                    labels[yy * width + x] = 1;
                }
            }
        }
        for &x in &[10usize, 50, 90] {
            for xx in x - 1..=x + 1 {
                for y in 10..=90 {
                    if labels[y * width + xx] == 0 {
                        labels[y * width + xx] = 2;
                    }
                }
            }
        }
        let output = TensorBundle {
            metadata: BTreeMap::new(),
            tensors: vec![Tensor {
                name: "segmentation".into(),
                dtype: TensorDType::I64,
                shape: vec![1, 1, height, width],
                data: labels
                    .iter()
                    .flat_map(|value| value.to_le_bytes())
                    .collect(),
            }],
        };
        let mut decoded = decode_unet(&output, (100, 100)).unwrap();
        assert_eq!(decoded.cells.len(), 4);
        assert_eq!((decoded.cells[3].row, decoded.cells[3].column), (1, 1));
        decoded.cells[0].text = "A&B".into();
        let html = render_wired_html(&decoded.cells);
        assert_eq!(html.matches("<td").count(), 4);
        assert!(html.contains("A&amp;B"));
    }
}
