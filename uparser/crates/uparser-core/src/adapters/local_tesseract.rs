//! Local Tesseract OCR adapter for rasterized pages.

use super::{ModelStage, ParseCtx, PostprocessSignals, ProtocolAdapter, RawOutputFormat};
use crate::ingest::RenderedPage;
use crate::types::{Block, BlockSource, CoordFrame, CoordinateSystem, Geometry, PageError, Span};
use async_trait::async_trait;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct TesseractAdapter {
    pub executable: PathBuf,
    pub language: String,
}

impl Default for TesseractAdapter {
    fn default() -> Self {
        Self {
            executable: executable_path().unwrap_or_else(|| PathBuf::from("tesseract")),
            language: std::env::var("UPARSER_OCR_LANG").unwrap_or_else(|_| "eng".to_owned()),
        }
    }
}

impl TesseractAdapter {
    pub fn with_language(language: impl Into<String>) -> Self {
        Self {
            executable: executable_path().unwrap_or_else(|| PathBuf::from("tesseract")),
            language: language.into(),
        }
    }

    pub fn has_language(&self) -> bool {
        self.language.split('+').all(|language| {
            self.executable
                .parent()
                .map(|parent| {
                    parent
                        .join("tessdata")
                        .join(format!("{language}.traineddata"))
                        .is_file()
                })
                .unwrap_or(language == "eng")
        })
    }

    pub async fn recognize_page(&self, page: &RenderedPage) -> Result<Vec<Block>, PageError> {
        let prepared =
            prepare_image(&page.png_bytes).map_err(|error| page_error(page.page_num, error))?;
        let mut source = tempfile::Builder::new()
            .suffix(".png")
            .tempfile()
            .map_err(|error| page_error(page.page_num, error.to_string()))?;
        source
            .write_all(&prepared)
            .map_err(|error| page_error(page.page_num, error.to_string()))?;
        let mut command = tokio::process::Command::new(&self.executable);
        command.args([
            source.path().as_os_str(),
            std::ffi::OsStr::new("stdout"),
            std::ffi::OsStr::new("-l"),
            std::ffi::OsStr::new(&self.language),
            std::ffi::OsStr::new("--oem"),
            std::ffi::OsStr::new("1"),
            std::ffi::OsStr::new("--psm"),
            std::ffi::OsStr::new("3"),
            std::ffi::OsStr::new("tsv"),
        ]);
        if let Some(tessdata) = self
            .executable
            .parent()
            .map(|parent| parent.join("tessdata"))
            && tessdata.is_dir()
        {
            command.env("TESSDATA_PREFIX", tessdata);
        }
        command.kill_on_drop(true);
        let output = command
            .output()
            .await
            .map_err(|error| page_error(page.page_num, error.to_string()))?;
        if !output.status.success() {
            return Err(page_error(
                page.page_num,
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        parse_tsv_blocks(&output.stdout, page).map_err(|error| page_error(page.page_num, error))
    }
}

#[derive(Debug, Deserialize)]
struct TsvRow {
    level: u8,
    block_num: u32,
    par_num: u32,
    line_num: u32,
    left: i32,
    top: i32,
    width: i32,
    height: i32,
    conf: f32,
    text: String,
}

#[derive(Default)]
struct OcrLine {
    words: Vec<(String, [i32; 4], f32)>,
}

fn parse_tsv_blocks(bytes: &[u8], page: &RenderedPage) -> Result<Vec<Block>, String> {
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(b'\t')
        .flexible(true)
        .from_reader(bytes);
    let mut lines = BTreeMap::<(u32, u32, u32), OcrLine>::new();
    for row in reader.deserialize::<TsvRow>() {
        let row = row.map_err(|error| format!("invalid Tesseract TSV: {error}"))?;
        if row.level != 5 || row.conf < 0.0 {
            continue;
        }
        let text = clean_ocr_text(row.text.trim());
        if text.is_empty() || row.width <= 0 || row.height <= 0 {
            continue;
        }
        let bbox = [
            row.left.max(0),
            row.top.max(0),
            (row.left + row.width).clamp(0, page.width as i32),
            (row.top + row.height).clamp(0, page.height as i32),
        ];
        lines
            .entry((row.block_num, row.par_num, row.line_num))
            .or_default()
            .words
            .push((text, bbox, row.conf));
    }

    let mut blocks = Vec::with_capacity(lines.len());
    for line in lines.into_values() {
        if line.words.is_empty() {
            continue;
        }
        let bbox = line.words.iter().fold(
            [i32::MAX, i32::MAX, i32::MIN, i32::MIN],
            |mut bbox, (_, word, _)| {
                bbox[0] = bbox[0].min(word[0]);
                bbox[1] = bbox[1].min(word[1]);
                bbox[2] = bbox[2].max(word[2]);
                bbox[3] = bbox[3].max(word[3]);
                bbox
            },
        );
        let text = join_ocr_words(line.words.iter().map(|(text, _, _)| text.as_str()));
        let confidence = line
            .words
            .iter()
            .map(|(_, _, confidence)| confidence)
            .sum::<f32>()
            / line.words.len() as f32
            / 100.0;
        let spans = line
            .words
            .iter()
            .map(|(text, bbox, _)| Span {
                // Tesseract reports no character styling.
                style: Default::default(),
                text: text.clone(),
                bbox_px: Some(*bbox),
                font_size: None,
                is_inline_formula: false,
            })
            .collect();
        blocks.push(Block {
            geom: Geometry::Rect([
                bbox[0] as f32,
                bbox[1] as f32,
                bbox[2] as f32,
                bbox[3] as f32,
            ]),
            geom_frame: CoordFrame::Page,
            bbox_px: Some(bbox),
            category_raw: "ocr_line".to_owned(),
            category: Some("text".to_owned()),
            reading_order: Some(blocks.len() as u32),
            text: Some(text),
            html: None,
            latex: None,
            spans,
            merge_hint: None,
            confidence: Some(confidence.clamp(0.0, 1.0)),
            source: BlockSource::OcrPipeline,
            error: None,
            asset_bytes: None,
            asset_path: None,
            asset_caption: None,
        });
    }
    Ok(blocks)
}

fn join_ocr_words<'a>(words: impl IntoIterator<Item = &'a str>) -> String {
    let mut joined = String::new();
    for word in words {
        if !joined.is_empty()
            && !joined.ends_with(char::is_whitespace)
            && !word.starts_with(is_closing_punctuation)
            && !joined.ends_with(is_opening_punctuation)
            && !(joined.chars().last().is_some_and(is_cjk)
                && word.chars().next().is_some_and(is_cjk))
        {
            joined.push(' ');
        }
        joined.push_str(word);
    }
    joined
}

fn is_cjk(character: char) -> bool {
    matches!(character, '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}')
}

fn is_closing_punctuation(character: char) -> bool {
    matches!(
        character,
        ',' | '.'
            | ':'
            | ';'
            | '!'
            | '?'
            | ')'
            | ']'
            | '}'
            | '，'
            | '。'
            | '：'
            | '；'
            | '！'
            | '？'
            | '）'
            | '】'
            | '》'
    )
}

fn is_opening_punctuation(character: char) -> bool {
    matches!(character, '(' | '[' | '{' | '（' | '【' | '《')
}

fn clean_ocr_text(text: &str) -> String {
    let mut cleaned = String::with_capacity(text.len());
    let mut noise = String::new();
    let flush_noise = |cleaned: &mut String, noise: &mut String| {
        if noise.chars().count() < 6 {
            cleaned.push_str(noise);
        } else if !cleaned.ends_with(char::is_whitespace) {
            cleaned.push(' ');
        }
        noise.clear();
    };
    for character in text.chars() {
        if matches!(
            character,
            'o' | 'O' | '0' | 'e' | 'E' | 's' | 'S' | 'c' | 'C' | '.' | '。' | '·'
        ) {
            noise.push(character);
        } else {
            flush_noise(&mut cleaned, &mut noise);
            cleaned.push(character);
        }
    }
    flush_noise(&mut cleaned, &mut noise);
    cleaned
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

pub fn available() -> bool {
    executable_path().is_some()
}

pub fn executable_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("UPARSER_TESSERACT_PATH").map(PathBuf::from)
        && path.is_file()
    {
        return Some(path);
    }
    if let Ok(executable) = std::env::current_exe() {
        for ancestor in executable.ancestors() {
            let bundled = ancestor
                .join("tools")
                .join("tesseract")
                .join("tesseract.exe");
            if bundled.is_file() {
                return Some(bundled);
            }
        }
    }
    #[cfg(windows)]
    for root in [
        std::env::var_os("ProgramFiles"),
        std::env::var_os("ProgramFiles(x86)"),
    ]
    .into_iter()
    .flatten()
    {
        let installed = PathBuf::from(root)
            .join("Tesseract-OCR")
            .join("tesseract.exe");
        if installed.is_file() {
            return Some(installed);
        }
    }
    let probe = if cfg!(windows) { "where" } else { "which" };
    std::process::Command::new(probe)
        .arg("tesseract")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|output| output.lines().next().map(str::trim).map(PathBuf::from))
}

#[async_trait]
impl ProtocolAdapter for TesseractAdapter {
    fn name(&self) -> &'static str {
        "tesseract"
    }

    fn coordinate_system(&self) -> CoordinateSystem {
        CoordinateSystem::PixelAbs
    }

    fn provides_reading_order(&self) -> bool {
        true
    }

    fn category_vocab(&self) -> &[&'static str] {
        &["text"]
    }

    fn raw_output_format(&self) -> RawOutputFormat {
        RawOutputFormat::None
    }

    fn emitted_signals(&self) -> PostprocessSignals {
        PostprocessSignals::default()
    }

    fn model_stages(&self) -> Vec<ModelStage> {
        vec![]
    }

    async fn parse_page(
        &self,
        page: &RenderedPage,
        _ctx: &ParseCtx,
    ) -> Result<Vec<Block>, PageError> {
        self.recognize_page(page).await
    }
}

fn prepare_image(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let source = image::load_from_memory(bytes)
        .map_err(|error| format!("OCR image decode failed: {error}"))?
        .to_rgba8();
    let grayscale = image::GrayImage::from_fn(source.width(), source.height(), |x, y| {
        let pixel = source.get_pixel(x, y);
        let alpha = u16::from(pixel[3]);
        let darkest = u16::from(pixel[0].min(pixel[1]).min(pixel[2]));
        let blended = (darkest * alpha + 255 * (255 - alpha)) / 255;
        image::Luma([blended as u8])
    });
    let mut png = Vec::new();
    image::DynamicImage::ImageLuma8(grayscale)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|error| format!("OCR image encode failed: {error}"))?;
    Ok(png)
}

fn page_error(page_num: u32, message: String) -> PageError {
    PageError {
        page_num,
        message: format!("local Tesseract OCR failed: {message}"),
        stage: Some("tesseract".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::MockDispatch;
    use std::sync::Arc;
    use tokio::sync::Semaphore;

    fn context() -> ParseCtx {
        ParseCtx::with_mock(
            Arc::new(MockDispatch::default()),
            Arc::new(Semaphore::new(1)),
        )
    }

    fn png() -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(2, 1, image::Rgba([10, 20, 30, 128]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(image)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    }

    #[test]
    fn metadata_and_image_preparation_are_stable() {
        let adapter = TesseractAdapter {
            executable: PathBuf::from("missing-tesseract"),
            language: "eng".to_owned(),
        };
        assert_eq!(adapter.name(), "tesseract");
        assert_eq!(adapter.coordinate_system(), CoordinateSystem::PixelAbs);
        assert!(adapter.provides_reading_order());
        assert_eq!(adapter.category_vocab(), ["text"]);
        assert_eq!(adapter.raw_output_format(), RawOutputFormat::None);
        assert!(adapter.model_stages().is_empty());
        assert!(!adapter.emitted_signals().spans);

        let prepared = prepare_image(&png()).expect("valid PNG");
        let grayscale = image::load_from_memory(&prepared).unwrap().to_luma8();
        assert_eq!(grayscale.dimensions(), (2, 1));
        assert!(grayscale.get_pixel(0, 0)[0] > 100);
        assert_eq!(
            clean_ocr_text("目录 oooooeeooooooo 5\nnormal text"),
            "目录  5\nnormal text"
        );
        let rendered = RenderedPage {
            page_num: 1,
            png_bytes: png(),
            width: 100,
            height: 80,
        };
        let tsv = concat!(
            "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n",
            "5\t1\t1\t1\t1\t1\t10\t20\t15\t10\t90.0\t3.1\n",
            "5\t1\t1\t1\t1\t2\t30\t20\t20\t10\t80.0\t安全\n",
            "5\t1\t1\t1\t2\t1\t10\t40\t40\t10\t70.0\t管理\n",
        );
        let blocks = parse_tsv_blocks(tsv.as_bytes(), &rendered).unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].text.as_deref(), Some("3.1 安全"));
        assert_eq!(blocks[0].bbox_px, Some([10, 20, 50, 30]));
        assert_eq!(blocks[0].spans.len(), 2);
        assert_eq!(blocks[0].confidence, Some(0.85));
        assert_eq!(blocks[1].reading_order, Some(1));

        let error = page_error(7, "boom".to_owned());
        assert_eq!(error.page_num, 7);
        assert_eq!(error.stage.as_deref(), Some("tesseract"));
        assert!(error.message.contains("boom"));
    }

    #[tokio::test]
    async fn invalid_images_and_missing_executables_are_typed_page_errors() {
        let adapter = TesseractAdapter {
            executable: PathBuf::from("definitely-not-a-real-tesseract-binary"),
            language: "eng".to_owned(),
        };
        let invalid = RenderedPage {
            page_num: 3,
            png_bytes: b"not an image".to_vec(),
            width: 1,
            height: 1,
        };
        let error = adapter.parse_page(&invalid, &context()).await.unwrap_err();
        assert_eq!(error.page_num, 3);
        assert!(error.message.contains("OCR image decode failed"));

        let valid = RenderedPage {
            page_num: 4,
            png_bytes: png(),
            width: 2,
            height: 1,
        };
        let error = adapter.parse_page(&valid, &context()).await.unwrap_err();
        assert_eq!(error.page_num, 4);
        assert!(error.message.contains("failed"));
    }
}
