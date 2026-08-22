//! Local Tesseract OCR adapter for rasterized pages.

use super::{ModelStage, ParseCtx, PostprocessSignals, ProtocolAdapter, RawOutputFormat};
use crate::ingest::RenderedPage;
use crate::types::{Block, BlockSource, CoordFrame, CoordinateSystem, Geometry, PageError};
use async_trait::async_trait;
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
        let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if text.is_empty() {
            return Ok(Vec::new());
        }
        Ok(vec![Block {
            geom: Geometry::Rect([0.0, 0.0, page.width as f32, page.height as f32]),
            geom_frame: CoordFrame::Page,
            bbox_px: Some([0, 0, page.width as i32, page.height as i32]),
            category_raw: "text".to_owned(),
            category: Some("text".to_owned()),
            reading_order: Some(0),
            text: Some(text),
            html: None,
            latex: None,
            spans: Vec::new(),
            merge_hint: None,
            confidence: None,
            source: BlockSource::OcrPipeline,
            error: None,
            asset_bytes: None,
            asset_path: None,
        }])
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
