//! Rust preprocessing and token decoding for PP-FormulaNet-plus-M.

use crate::tensor_wire::{Tensor, TensorBundle, TensorDType, TensorWireError};
use image::imageops::FilterType;
use regex::Regex;
use serde_yaml::Value;
use std::collections::BTreeMap;
use std::path::Path;
use tokenizers::Tokenizer;

const INPUT_SIZE: u32 = 384;
const MEAN: f32 = 0.7931;
const STD: f32 = 0.1738;

#[derive(Debug, thiserror::Error)]
pub enum FormulaError {
    #[error(transparent)]
    Tensor(#[from] TensorWireError),
    #[error("cannot decode formula image: {0}")]
    Image(#[from] image::ImageError),
    #[error("cannot read formula inference config: {0}")]
    ConfigIo(#[from] std::io::Error),
    #[error("invalid formula inference YAML: {0}")]
    ConfigYaml(#[from] serde_yaml::Error),
    #[error("formula inference YAML is missing PostProcess.character_dict.fast_tokenizer_file")]
    MissingTokenizer,
    #[error("invalid formula tokenizer: {0}")]
    Tokenizer(String),
    #[error("missing formula tensor: token_ids")]
    MissingTokenIds,
    #[error("formula token_ids has dtype {0:?}; expected I64")]
    InvalidTokenDType(TensorDType),
    #[error("invalid formula token_ids shape {0:?}; expected [batch,sequence]")]
    InvalidTokenShape(Vec<usize>),
}

pub struct FormulaDecoder {
    tokenizer: Tokenizer,
}

impl FormulaDecoder {
    pub fn from_inference_yaml(path: impl AsRef<Path>) -> Result<Self, FormulaError> {
        let source = std::fs::read_to_string(path)?;
        let root: Value = serde_yaml::from_str(&source)?;
        let tokenizer = root
            .get("PostProcess")
            .and_then(|value| value.get("character_dict"))
            .and_then(|value| value.get("fast_tokenizer_file"))
            .ok_or(FormulaError::MissingTokenizer)?;
        let json = serde_json::to_string(tokenizer)
            .map_err(|error| FormulaError::Tokenizer(error.to_string()))?;
        let tokenizer = Tokenizer::from_bytes(json.as_bytes())
            .map_err(|error| FormulaError::Tokenizer(error.to_string()))?;
        Ok(Self { tokenizer })
    }

    pub fn decode(&self, outputs: &TensorBundle) -> Result<Vec<String>, FormulaError> {
        let tensor = outputs
            .tensors
            .iter()
            .find(|tensor| tensor.name == "token_ids")
            .ok_or(FormulaError::MissingTokenIds)?;
        if tensor.dtype != TensorDType::I64 {
            return Err(FormulaError::InvalidTokenDType(tensor.dtype));
        }
        if tensor.shape.len() != 2 {
            return Err(FormulaError::InvalidTokenShape(tensor.shape.clone()));
        }
        let batch = tensor.shape[0];
        let sequence = tensor.shape[1];
        let ids: Vec<i64> = tensor
            .data
            .chunks_exact(8)
            .map(|bytes| i64::from_le_bytes(bytes.try_into().expect("eight-byte chunk")))
            .collect();
        if ids.len() != batch * sequence {
            return Err(FormulaError::InvalidTokenShape(tensor.shape.clone()));
        }
        ids.chunks(sequence)
            .map(|row| {
                let end = row
                    .iter()
                    .position(|&id| id == 2)
                    .map_or(row.len(), |index| index + 1);
                let ids: Result<Vec<u32>, FormulaError> = row[..end]
                    .iter()
                    .map(|&id| {
                        u32::try_from(id)
                            .map_err(|_| FormulaError::Tokenizer(format!("invalid token id {id}")))
                    })
                    .collect();
                let text = self
                    .tokenizer
                    .decode(&ids?, true)
                    .map_err(|error| FormulaError::Tokenizer(error.to_string()))?;
                Ok(fix_latex(&remove_chinese_text_wrapping(&text)))
            })
            .collect()
    }
}

pub fn preprocess(image_bytes: &[u8]) -> Result<TensorBundle, FormulaError> {
    let image = image::load_from_memory(image_bytes)?.to_rgb8();
    let gray = image::DynamicImage::ImageRgb8(image).to_luma8();
    let (width, height) = gray.dimensions();
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (width, height, 0, 0);
    let (min_value, max_value) = gray.pixels().fold((u8::MAX, u8::MIN), |(min, max), pixel| {
        (min.min(pixel[0]), max.max(pixel[0]))
    });
    if min_value != max_value {
        let range = f32::from(max_value - min_value);
        for (x, y, pixel) in gray.enumerate_pixels() {
            let normalized = f32::from(pixel[0] - min_value) / range * 255.0;
            if normalized < 200.0 {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }
    let cropped = if min_x <= max_x && min_y <= max_y {
        image::imageops::crop_imm(&gray, min_x, min_y, max_x - min_x + 1, max_y - min_y + 1)
            .to_image()
    } else {
        gray
    };

    // MinerU first resizes the short edge with Pillow bilinear, then calls
    // thumbnail (bicubic) to constrain the long edge.
    let (crop_width, crop_height) = cropped.dimensions();
    let short_scale = INPUT_SIZE as f64 / f64::from(crop_width.min(crop_height));
    let first_width = (f64::from(crop_width) * short_scale).floor().max(1.0) as u32;
    let first_height = (f64::from(crop_height) * short_scale).floor().max(1.0) as u32;
    let first = image::imageops::resize(&cropped, first_width, first_height, FilterType::Triangle);
    let long_scale = (INPUT_SIZE as f64 / f64::from(first_width.max(first_height))).min(1.0);
    let final_width = (f64::from(first_width) * long_scale).floor().max(1.0) as u32;
    let final_height = (f64::from(first_height) * long_scale).floor().max(1.0) as u32;
    let resized =
        image::imageops::resize(&first, final_width, final_height, FilterType::CatmullRom);

    let offset_x = (INPUT_SIZE - final_width) / 2;
    let offset_y = (INPUT_SIZE - final_height) / 2;
    let padding_value = (0.0 / 255.0 - MEAN) / STD;
    let mut values = vec![padding_value; (INPUT_SIZE * INPUT_SIZE) as usize];
    for (x, y, pixel) in resized.enumerate_pixels() {
        values[((y + offset_y) * INPUT_SIZE + x + offset_x) as usize] =
            (f32::from(pixel[0]) / 255.0 - MEAN) / STD;
    }
    Ok(TensorBundle {
        metadata: BTreeMap::new(),
        tensors: vec![Tensor::from_f32(
            "pixel_values",
            vec![1, 1, INPUT_SIZE as usize, INPUT_SIZE as usize],
            &values,
        )],
    })
}

fn remove_chinese_text_wrapping(text: &str) -> String {
    let pattern = Regex::new(r#"\\text\s*\{\s*([^}]*[\p{Han}][^}]*)\s*\}"#).unwrap();
    pattern.replace_all(text, "$1").replace('"', "")
}

fn fix_latex(text: &str) -> String {
    fn command_count(text: &str, command: &str) -> usize {
        text.match_indices(command)
            .filter(|(index, _)| {
                text[*index + command.len()..]
                    .chars()
                    .next()
                    .is_none_or(|next| !next.is_ascii_alphabetic())
            })
            .count()
    }
    let mut text = if command_count(text, r"\left") != command_count(text, r"\right") {
        Regex::new(r"\\left\.?|\\right\.?")
            .unwrap()
            .replace_all(text, "")
            .into_owned()
    } else {
        text.to_owned()
    };
    for environment in [
        "array", "matrix", "pmatrix", "bmatrix", "vmatrix", "Bmatrix", "Vmatrix", "cases",
        "aligned", "gathered", "align", "align*",
    ] {
        let begin = format!(r"\begin{{{environment}}}");
        let end = format!(r"\end{{{environment}}}");
        let begin_count = text.matches(&begin).count();
        let end_count = text.matches(&end).count();
        if begin_count < end_count {
            let format = if environment == "array" { "{c}" } else { "" };
            text = format!(
                "{}{}",
                format!("{begin}{format} ").repeat(end_count - begin_count),
                text
            );
        } else if begin_count > end_count {
            text.push_str(&format!(" {end}").repeat(begin_count - end_count));
        }
    }
    text = Regex::new(r"\\up([a-zA-Z]+)")
        .unwrap()
        .replace_all(&text, |captures: &regex::Captures<'_>| match &captures[1] {
            "arrow" | "downarrow" | "lus" | "silon" => captures[0].to_owned(),
            command => format!(r"\{command}"),
        })
        .into_owned();
    Regex::new(
        r"\\(?:lefteqn|boldmath|ensuremath|centering|textsubscript|sides|textsl|textcent|emph|protect|null)",
    )
    .unwrap()
    .replace_all(&text, "")
    .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, ImageFormat, Luma};
    use std::io::Cursor;

    #[test]
    fn preprocess_crops_and_centers_formula() {
        let mut image = ImageBuffer::from_pixel(80, 40, Luma([255u8]));
        for x in 20..60 {
            image.put_pixel(x, 15, Luma([0]));
            image.put_pixel(x, 24, Luma([0]));
        }
        for y in 15..25 {
            image.put_pixel(20, y, Luma([0]));
            image.put_pixel(59, y, Luma([0]));
        }
        let mut encoded = Vec::new();
        image::DynamicImage::ImageLuma8(image)
            .write_to(&mut Cursor::new(&mut encoded), ImageFormat::Png)
            .unwrap();
        let bundle = preprocess(&encoded).unwrap();
        assert_eq!(bundle.tensors[0].shape, [1, 1, 384, 384]);
        let values = bundle.tensors[0].to_f32().unwrap();
        assert!(values.iter().any(|value| *value < -4.0));
        assert!(values.iter().any(|value| *value > 1.0));
    }

    #[test]
    fn latex_cleanup_matches_mineru_rules() {
        assert_eq!(fix_latex(r"\left(x+1"), "(x+1");
        assert_eq!(fix_latex(r"\upalpha+\uparrow"), r"\alpha+\uparrow");
        assert_eq!(remove_chinese_text_wrapping(r#"\text{速度 v}"#), "速度 v");
    }
}
