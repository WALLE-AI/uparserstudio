//! Rust preprocessing and CTC decoding for PP-OCRv6 recognition.

use crate::tensor_wire::{Tensor, TensorBundle, TensorDType, TensorWireError};
use std::collections::BTreeMap;
use std::io::BufRead;

#[derive(Debug, Clone, PartialEq)]
pub struct CtcRecognition {
    pub text: String,
    pub confidence: f32,
}

#[derive(Debug, Clone)]
pub struct DetectionInput {
    pub tensors: TensorBundle,
    pub original_size: (u32, u32),
    pub resized_size: (u32, u32),
}

#[derive(Debug, Clone, PartialEq)]
pub struct OcrDetection {
    pub points: [[f32; 2]; 4],
    pub confidence: f32,
}

#[derive(Debug, Clone, Copy)]
struct RotatedRect {
    center: [f32; 2],
    axis: [f32; 2],
    normal: [f32; 2],
    width: f32,
    height: f32,
}

impl RotatedRect {
    fn points(self, expansion: f32) -> [[f32; 2]; 4] {
        let half_width = self.width * 0.5 + expansion;
        let half_height = self.height * 0.5 + expansion;
        [
            [-half_width, -half_height],
            [half_width, -half_height],
            [half_width, half_height],
            [-half_width, half_height],
        ]
        .map(|[along, across]| {
            [
                self.center[0] + self.axis[0] * along + self.normal[0] * across,
                self.center[1] + self.axis[1] * along + self.normal[1] * across,
            ]
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OcrError {
    #[error(transparent)]
    Tensor(#[from] TensorWireError),
    #[error("missing OCR tensor: {0}")]
    MissingTensor(&'static str),
    #[error("invalid OCR logits shape {0:?}; expected [batch,time,classes]")]
    InvalidLogitsShape(Vec<usize>),
    #[error("invalid OCR detector map shape {0:?}; expected [1,1,height,width]")]
    InvalidMapShape(Vec<usize>),
    #[error("OCR logits expose {actual} classes, but dictionary requires {expected}")]
    DictionarySize { actual: usize, expected: usize },
    #[error("OCR recognition crop has zero width or height")]
    EmptyCrop,
    #[error("cannot read OCR dictionary: {0}")]
    DictionaryIo(#[from] std::io::Error),
}

fn cross(origin: [i32; 2], left: [i32; 2], right: [i32; 2]) -> i64 {
    i64::from(left[0] - origin[0]) * i64::from(right[1] - origin[1])
        - i64::from(left[1] - origin[1]) * i64::from(right[0] - origin[0])
}

fn convex_hull(mut points: Vec<[i32; 2]>) -> Vec<[i32; 2]> {
    points.sort_unstable();
    points.dedup();
    if points.len() <= 2 {
        return points;
    }
    let mut lower = Vec::new();
    for &point in &points {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], point) <= 0
        {
            lower.pop();
        }
        lower.push(point);
    }
    let mut upper = Vec::new();
    for &point in points.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], point) <= 0
        {
            upper.pop();
        }
        upper.push(point);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

fn minimum_rotated_rect(hull: &[[i32; 2]]) -> Option<RotatedRect> {
    if hull.len() < 3 {
        return None;
    }
    let mut best: Option<(f32, RotatedRect)> = None;
    for index in 0..hull.len() {
        let start = hull[index];
        let end = hull[(index + 1) % hull.len()];
        let dx = (end[0] - start[0]) as f32;
        let dy = (end[1] - start[1]) as f32;
        let length = dx.hypot(dy);
        if length <= f32::EPSILON {
            continue;
        }
        let axis = [dx / length, dy / length];
        let normal = [-axis[1], axis[0]];
        let mut min_axis = f32::INFINITY;
        let mut max_axis = f32::NEG_INFINITY;
        let mut min_normal = f32::INFINITY;
        let mut max_normal = f32::NEG_INFINITY;
        for &[x, y] in hull {
            let along = x as f32 * axis[0] + y as f32 * axis[1];
            let across = x as f32 * normal[0] + y as f32 * normal[1];
            min_axis = min_axis.min(along);
            max_axis = max_axis.max(along);
            min_normal = min_normal.min(across);
            max_normal = max_normal.max(across);
        }
        let width = max_axis - min_axis;
        let height = max_normal - min_normal;
        let center_axis = (min_axis + max_axis) * 0.5;
        let center_normal = (min_normal + max_normal) * 0.5;
        let rect = RotatedRect {
            center: [
                axis[0] * center_axis + normal[0] * center_normal,
                axis[1] * center_axis + normal[1] * center_normal,
            ],
            axis,
            normal,
            width,
            height,
        };
        let candidate = (width * height, rect);
        if best.is_none_or(|(area, _)| candidate.0 < area) {
            best = Some(candidate);
        }
    }
    best.map(|(_, rect)| rect)
}

fn connected_components(bitmap: &[bool], width: usize, height: usize) -> Vec<Vec<[i32; 2]>> {
    let mut visited = vec![false; bitmap.len()];
    let mut components = Vec::new();
    for seed in 0..bitmap.len() {
        if !bitmap[seed] || visited[seed] {
            continue;
        }
        visited[seed] = true;
        let mut stack = vec![seed];
        let mut points = Vec::new();
        while let Some(index) = stack.pop() {
            let x = index % width;
            let y = index / width;
            points.push([x as i32, y as i32]);
            for offset_y in -1i32..=1 {
                for offset_x in -1i32..=1 {
                    if offset_x == 0 && offset_y == 0 {
                        continue;
                    }
                    let neighbor_x = x as i32 + offset_x;
                    let neighbor_y = y as i32 + offset_y;
                    if neighbor_x < 0
                        || neighbor_y < 0
                        || neighbor_x >= width as i32
                        || neighbor_y >= height as i32
                    {
                        continue;
                    }
                    let neighbor = neighbor_y as usize * width + neighbor_x as usize;
                    if bitmap[neighbor] && !visited[neighbor] {
                        visited[neighbor] = true;
                        stack.push(neighbor);
                    }
                }
            }
        }
        components.push(points);
    }
    components
}

fn point_in_polygon(point: [f32; 2], polygon: &[[f32; 2]; 4]) -> bool {
    let mut sign = 0i8;
    for index in 0..4 {
        let start = polygon[index];
        let end = polygon[(index + 1) % 4];
        let cross = (end[0] - start[0]) * (point[1] - start[1])
            - (end[1] - start[1]) * (point[0] - start[0]);
        if cross.abs() <= 1e-4 {
            continue;
        }
        let current = if cross > 0.0 { 1 } else { -1 };
        if sign != 0 && sign != current {
            return false;
        }
        sign = current;
    }
    true
}

fn box_score(map: &[f32], width: usize, height: usize, polygon: &[[f32; 2]; 4]) -> f32 {
    let min_x = polygon
        .iter()
        .map(|point| point[0])
        .fold(f32::INFINITY, f32::min)
        .floor()
        .clamp(0.0, width.saturating_sub(1) as f32) as usize;
    let max_x = polygon
        .iter()
        .map(|point| point[0])
        .fold(f32::NEG_INFINITY, f32::max)
        .ceil()
        .clamp(0.0, width.saturating_sub(1) as f32) as usize;
    let min_y = polygon
        .iter()
        .map(|point| point[1])
        .fold(f32::INFINITY, f32::min)
        .floor()
        .clamp(0.0, height.saturating_sub(1) as f32) as usize;
    let max_y = polygon
        .iter()
        .map(|point| point[1])
        .fold(f32::NEG_INFINITY, f32::max)
        .ceil()
        .clamp(0.0, height.saturating_sub(1) as f32) as usize;
    let mut sum = 0.0f32;
    let mut count = 0usize;
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            if point_in_polygon([x as f32, y as f32], polygon) {
                sum += map[y * width + x];
                count += 1;
            }
        }
    }
    if count == 0 { 0.0 } else { sum / count as f32 }
}

fn order_box(points: [[f32; 2]; 4]) -> [[f32; 2]; 4] {
    let mut by_x = points;
    by_x.sort_by(|left, right| left[0].total_cmp(&right[0]));
    let mut left = [by_x[0], by_x[1]];
    let mut right = [by_x[2], by_x[3]];
    left.sort_by(|top, bottom| top[1].total_cmp(&bottom[1]));
    right.sort_by(|top, bottom| top[1].total_cmp(&bottom[1]));
    [left[0], right[0], right[1], left[1]]
}

pub fn decode_detection(
    bundle: &TensorBundle,
    original_size: (u32, u32),
    bitmap_threshold: f32,
    box_threshold: f32,
    unclip_ratio: f32,
) -> Result<Vec<OcrDetection>, OcrError> {
    let tensor = bundle
        .tensors
        .iter()
        .find(|tensor| tensor.name == "maps")
        .ok_or(OcrError::MissingTensor("maps"))?;
    if tensor.dtype != TensorDType::F32 {
        return Err(OcrError::Tensor(TensorWireError::UnexpectedDType {
            name: tensor.name.clone(),
            expected: TensorDType::F32,
            actual: tensor.dtype,
        }));
    }
    if tensor.shape.len() != 4 || tensor.shape[0] != 1 || tensor.shape[1] != 1 {
        return Err(OcrError::InvalidMapShape(tensor.shape.clone()));
    }
    let height = tensor.shape[2];
    let width = tensor.shape[3];
    let map = tensor.to_f32()?;
    let bitmap: Vec<bool> = map.iter().map(|value| *value > bitmap_threshold).collect();
    let mut detections = Vec::new();
    for component in connected_components(&bitmap, width, height)
        .into_iter()
        .take(1000)
    {
        let Some(rect) = minimum_rotated_rect(&convex_hull(component)) else {
            continue;
        };
        if rect.width.min(rect.height) < 3.0 {
            continue;
        }
        let initial = rect.points(0.0);
        let confidence = box_score(&map, width, height, &initial);
        if confidence < box_threshold {
            continue;
        }
        let area = rect.width * rect.height;
        let perimeter = 2.0 * (rect.width + rect.height);
        if perimeter <= f32::EPSILON {
            continue;
        }
        let expansion = area * unclip_ratio / perimeter;
        if (rect.width + expansion * 2.0).min(rect.height + expansion * 2.0) < 5.0 {
            continue;
        }
        let scaled = rect.points(expansion).map(|point| {
            [
                (point[0] / width as f32 * original_size.0 as f32)
                    .round()
                    .clamp(0.0, original_size.0 as f32),
                (point[1] / height as f32 * original_size.1 as f32)
                    .round()
                    .clamp(0.0, original_size.1 as f32),
            ]
        });
        let points = order_box(scaled);
        let box_width = (points[0][0] - points[1][0]).hypot(points[0][1] - points[1][1]);
        let box_height = (points[0][0] - points[3][0]).hypot(points[0][1] - points[3][1]);
        if box_width <= 3.0 || box_height <= 3.0 {
            continue;
        }
        detections.push(OcrDetection { points, confidence });
    }
    detections.sort_by(|left, right| {
        left.points[0][1]
            .total_cmp(&right.points[0][1])
            .then_with(|| left.points[0][0].total_cmp(&right.points[0][0]))
    });
    for index in 0..detections.len().saturating_sub(1) {
        let mut cursor = index;
        while cursor > 0
            && (detections[cursor + 1].points[0][1] - detections[cursor].points[0][1]).abs() < 10.0
            && detections[cursor + 1].points[0][0] < detections[cursor].points[0][0]
        {
            detections.swap(cursor, cursor + 1);
            cursor -= 1;
        }
    }
    Ok(detections)
}

#[derive(Debug, Clone)]
pub struct CtcDictionary {
    characters: Vec<String>,
}

impl CtcDictionary {
    pub fn from_reader(reader: impl BufRead) -> Result<Self, OcrError> {
        let mut characters = vec!["blank".to_owned()];
        for line in reader.lines() {
            characters.push(line?.trim_end_matches('\r').to_owned());
        }
        characters.push(" ".to_owned());
        Ok(Self { characters })
    }

    pub fn from_path(path: impl AsRef<std::path::Path>) -> Result<Self, OcrError> {
        let file = std::fs::File::open(path)?;
        Self::from_reader(std::io::BufReader::new(file))
    }

    pub fn len(&self) -> usize {
        self.characters.len()
    }

    pub fn is_empty(&self) -> bool {
        self.characters.is_empty()
    }
}

fn bilinear_sample(image: &image::RgbImage, x: f32, y: f32, channel: usize) -> u8 {
    let max_x = image.width().saturating_sub(1) as i32;
    let max_y = image.height().saturating_sub(1) as i32;
    let base_x = x.floor() as i32;
    let base_y = y.floor() as i32;
    let x0 = base_x.clamp(0, max_x);
    let y0 = base_y.clamp(0, max_y);
    let x1 = (base_x + 1).clamp(0, max_x);
    let y1 = (base_y + 1).clamp(0, max_y);
    let wx = x - x.floor();
    let wy = y - y.floor();
    let top = f32::from(image.get_pixel(x0 as u32, y0 as u32)[channel]) * (1.0 - wx)
        + f32::from(image.get_pixel(x1 as u32, y0 as u32)[channel]) * wx;
    let bottom = f32::from(image.get_pixel(x0 as u32, y1 as u32)[channel]) * (1.0 - wx)
        + f32::from(image.get_pixel(x1 as u32, y1 as u32)[channel]) * wx;
    (top * (1.0 - wy) + bottom * wy).round().clamp(0.0, 255.0) as u8
}

pub fn crop_detection(
    image: &image::RgbImage,
    detection: &OcrDetection,
) -> Result<image::RgbImage, OcrError> {
    let points = detection.points;
    let top_width = (points[1][0] - points[0][0]).hypot(points[1][1] - points[0][1]);
    let bottom_width = (points[2][0] - points[3][0]).hypot(points[2][1] - points[3][1]);
    let left_height = (points[3][0] - points[0][0]).hypot(points[3][1] - points[0][1]);
    let right_height = (points[2][0] - points[1][0]).hypot(points[2][1] - points[1][1]);
    let width = top_width.max(bottom_width).round().max(1.0) as u32;
    let height = left_height.max(right_height).round().max(1.0) as u32;
    let mut crop = image::RgbImage::new(width, height);
    for y in 0..height {
        let vertical = if height <= 1 {
            0.0
        } else {
            y as f32 / (height - 1) as f32
        };
        for x in 0..width {
            let horizontal = if width <= 1 {
                0.0
            } else {
                x as f32 / (width - 1) as f32
            };
            let top = [
                points[0][0] * (1.0 - horizontal) + points[1][0] * horizontal,
                points[0][1] * (1.0 - horizontal) + points[1][1] * horizontal,
            ];
            let bottom = [
                points[3][0] * (1.0 - horizontal) + points[2][0] * horizontal,
                points[3][1] * (1.0 - horizontal) + points[2][1] * horizontal,
            ];
            let source_x = top[0] * (1.0 - vertical) + bottom[0] * vertical;
            let source_y = top[1] * (1.0 - vertical) + bottom[1] * vertical;
            crop.put_pixel(
                x,
                y,
                image::Rgb([
                    bilinear_sample(image, source_x, source_y, 0),
                    bilinear_sample(image, source_x, source_y, 1),
                    bilinear_sample(image, source_x, source_y, 2),
                ]),
            );
        }
    }
    if height as f32 / width as f32 >= 1.5 {
        Ok(image::imageops::rotate90(&crop))
    } else {
        Ok(crop)
    }
}

/// PP-OCRv6 DB detector preprocessing for an RGB crop. The tensor is BGR,
/// matching MinerU's OpenCV path, and dimensions are rounded to multiples of 32.
pub fn preprocess_detection(image: &image::RgbImage) -> Result<DetectionInput, OcrError> {
    if image.width() == 0 || image.height() == 0 {
        return Err(OcrError::EmptyCrop);
    }
    let width = image.width() as f64;
    let height = image.height() as f64;
    let ratio = if width.max(height) > 960.0 {
        960.0 / width.max(height)
    } else {
        1.0
    };
    let mut resized_width = (width * ratio) as u32;
    let mut resized_height = (height * ratio) as u32;
    if resized_width.max(resized_height) > 4000 {
        let correction = 4000.0 / f64::from(resized_width.max(resized_height));
        resized_width = (f64::from(resized_width) * correction) as u32;
        resized_height = (f64::from(resized_height) * correction) as u32;
    }
    resized_width = (((f64::from(resized_width) / 32.0).round_ties_even() as u32) * 32).max(32);
    resized_height = (((f64::from(resized_height) / 32.0).round_ties_even() as u32) * 32).max(32);

    let mean = [0.485f32, 0.456, 0.406];
    let std = [0.229f32, 0.224, 0.225];
    let plane = resized_width as usize * resized_height as usize;
    let mut values = vec![0.0f32; plane * 3];
    for y in 0..resized_height as usize {
        let source_y = (y as f32 + 0.5) * image.height() as f32 / resized_height as f32 - 0.5;
        for x in 0..resized_width as usize {
            let source_x = (x as f32 + 0.5) * image.width() as f32 / resized_width as f32 - 0.5;
            for channel in 0..3 {
                let bgr_channel = 2 - channel;
                let pixel = bilinear_sample(image, source_x, source_y, bgr_channel);
                values[channel * plane + y * resized_width as usize + x] =
                    (f32::from(pixel) / 255.0 - mean[channel]) / std[channel];
            }
        }
    }
    Ok(DetectionInput {
        tensors: TensorBundle {
            metadata: BTreeMap::from([
                ("model".into(), "pp_ocrv6_det".into()),
                ("original_width".into(), image.width().to_string()),
                ("original_height".into(), image.height().to_string()),
                ("resized_width".into(), resized_width.to_string()),
                ("resized_height".into(), resized_height.to_string()),
            ]),
            tensors: vec![Tensor::from_f32(
                "pixel_values",
                vec![1, 3, resized_height as usize, resized_width as usize],
                &values,
            )],
        },
        original_size: (image.width(), image.height()),
        resized_size: (resized_width, resized_height),
    })
}

/// Build one recognition batch using PaddleOCR's 3x48 dynamic-width input.
/// Input images are RGB; channel order is reversed to match OpenCV BGR crops.
pub fn preprocess_recognition(crops: &[image::RgbImage]) -> Result<TensorBundle, OcrError> {
    if crops.is_empty() {
        return Ok(TensorBundle {
            metadata: BTreeMap::new(),
            tensors: vec![Tensor::from_f32("pixel_values", vec![0, 3, 48, 320], &[])],
        });
    }
    if crops
        .iter()
        .any(|crop| crop.width() == 0 || crop.height() == 0)
    {
        return Err(OcrError::EmptyCrop);
    }
    let max_ratio = crops
        .iter()
        .map(|crop| crop.width() as f32 / crop.height() as f32)
        .fold(320.0 / 48.0, f32::max);
    let batch_width = ((48.0 * max_ratio) as usize).clamp(16, 2560);
    let plane = 48 * batch_width;
    let mut values = vec![0.0f32; crops.len() * 3 * plane];

    for (batch, crop) in crops.iter().enumerate() {
        let ratio = crop.width() as f32 / crop.height() as f32;
        let resized_width = ((48.0 * ratio).ceil() as usize).max(16).min(batch_width);
        for y in 0..48 {
            let source_y = (y as f32 + 0.5) * crop.height() as f32 / 48.0 - 0.5;
            for x in 0..resized_width {
                let source_x = (x as f32 + 0.5) * crop.width() as f32 / resized_width as f32 - 0.5;
                for channel in 0..3 {
                    let bgr_channel = 2 - channel;
                    let pixel = bilinear_sample(crop, source_x, source_y, bgr_channel);
                    let offset = batch * 3 * plane + channel * plane + y * batch_width + x;
                    values[offset] = f32::from(pixel) / 127.5 - 1.0;
                }
            }
        }
    }
    Ok(TensorBundle {
        metadata: BTreeMap::from([("model".into(), "pp_ocrv6_rec".into())]),
        tensors: vec![Tensor::from_f32(
            "pixel_values",
            vec![crops.len(), 3, 48, batch_width],
            &values,
        )],
    })
}

pub fn decode_ctc(
    bundle: &TensorBundle,
    dictionary: &CtcDictionary,
) -> Result<Vec<CtcRecognition>, OcrError> {
    let tensor = bundle
        .tensors
        .iter()
        .find(|tensor| tensor.name == "ctc_logits")
        .ok_or(OcrError::MissingTensor("ctc_logits"))?;
    if tensor.dtype != TensorDType::F32 {
        return Err(OcrError::Tensor(TensorWireError::UnexpectedDType {
            name: tensor.name.clone(),
            expected: TensorDType::F32,
            actual: tensor.dtype,
        }));
    }
    if tensor.shape.len() != 3 {
        return Err(OcrError::InvalidLogitsShape(tensor.shape.clone()));
    }
    let [batch, time, classes]: [usize; 3] = tensor.shape.as_slice().try_into().unwrap();
    if classes != dictionary.len() {
        return Err(OcrError::DictionarySize {
            actual: classes,
            expected: dictionary.len(),
        });
    }
    let logits = tensor.to_f32()?;
    let mut recognitions = Vec::with_capacity(batch);
    for batch_index in 0..batch {
        let mut text = String::new();
        let mut probabilities = Vec::new();
        let mut previous = usize::MAX;
        for step in 0..time {
            let start = (batch_index * time + step) * classes;
            let row = &logits[start..start + classes];
            let (class, &max_logit) = row
                .iter()
                .enumerate()
                .max_by(|left, right| left.1.total_cmp(right.1))
                .expect("validated non-empty class dictionary");
            if class != 0 && class != previous {
                let sum = row
                    .iter()
                    .map(|value| (*value - max_logit).exp())
                    .sum::<f32>();
                probabilities.push(1.0 / sum);
                text.push_str(&dictionary.characters[class]);
            }
            previous = class;
        }
        let confidence = if probabilities.is_empty() {
            1.0
        } else {
            probabilities.iter().sum::<f32>() / probabilities.len() as f32
        };
        recognitions.push(CtcRecognition { text, confidence });
    }
    Ok(recognitions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctc_decode_removes_blank_and_repeated_classes() {
        let dictionary = CtcDictionary::from_reader("a\nb\n".as_bytes()).unwrap();
        let classes = dictionary.len();
        let ids = [1usize, 1, 0, 2, 2];
        let mut logits = vec![-10.0f32; ids.len() * classes];
        for (step, class) in ids.into_iter().enumerate() {
            logits[step * classes + class] = 10.0;
        }
        let bundle = TensorBundle {
            metadata: BTreeMap::new(),
            tensors: vec![Tensor::from_f32(
                "ctc_logits",
                vec![1, ids.len(), classes],
                &logits,
            )],
        };
        let decoded = decode_ctc(&bundle, &dictionary).unwrap();
        assert_eq!(decoded[0].text, "ab");
        assert!(decoded[0].confidence > 0.99);
    }

    #[test]
    fn recognition_preprocess_uses_dynamic_width_and_bgr_order() {
        let crop = image::RgbImage::from_pixel(20, 10, image::Rgb([255, 127, 0]));
        let bundle = preprocess_recognition(&[crop]).unwrap();
        let tensor = &bundle.tensors[0];
        assert_eq!(tensor.shape, [1, 3, 48, 320]);
        let values = tensor.to_f32().unwrap();
        assert!((values[0] + 1.0).abs() < 1e-6);
        assert!((values[48 * 320] - (127.0 / 127.5 - 1.0)).abs() < 1e-6);
        assert!((values[2 * 48 * 320] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn detection_preprocess_limits_long_side_and_uses_bgr_normalization() {
        let image = image::RgbImage::from_pixel(2000, 1000, image::Rgb([255, 127, 0]));
        let input = preprocess_detection(&image).unwrap();
        assert_eq!(input.original_size, (2000, 1000));
        assert_eq!(input.resized_size, (960, 480));
        let tensor = &input.tensors.tensors[0];
        assert_eq!(tensor.shape, [1, 3, 480, 960]);
        let values = tensor.to_f32().unwrap();
        let plane = 480 * 960;
        assert!((values[0] - (0.0 - 0.485) / 0.229).abs() < 1e-5);
        assert!((values[plane] - (127.0 / 255.0 - 0.456) / 0.224).abs() < 1e-5);
        assert!((values[2 * plane] - (1.0 - 0.406) / 0.225).abs() < 1e-5);
    }

    #[test]
    fn db_decode_extracts_and_unclips_a_confident_component() {
        let mut map = vec![0.0f32; 32 * 32];
        for y in 10..16 {
            for x in 5..25 {
                map[y * 32 + x] = 0.95;
            }
        }
        let bundle = TensorBundle {
            metadata: BTreeMap::new(),
            tensors: vec![Tensor::from_f32("maps", vec![1, 1, 32, 32], &map)],
        };
        let detections = decode_detection(&bundle, (320, 320), 0.3, 0.5, 1.5).unwrap();
        assert_eq!(detections.len(), 1);
        assert!(detections[0].confidence > 0.9);
        assert!(detections[0].points[0][0] < 50.0);
        assert!(detections[0].points[2][0] > 250.0);
    }

    #[test]
    fn rotated_detection_crop_turns_vertical_text_horizontal() {
        let image = image::RgbImage::from_fn(40, 80, |x, y| image::Rgb([x as u8, y as u8, 0]));
        let detection = OcrDetection {
            points: [[10.0, 5.0], [30.0, 5.0], [30.0, 75.0], [10.0, 75.0]],
            confidence: 1.0,
        };
        let crop = crop_detection(&image, &detection).unwrap();
        assert!(crop.width() > crop.height());
    }
}
