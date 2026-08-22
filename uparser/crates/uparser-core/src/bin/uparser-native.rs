//! Minimal zero-model PDF/Office to Markdown CLI.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str =
    "usage: uparser-native [parse] <file> [--output <file>] [--no-ocr] [--ocr-lang <lang>]";

struct CliOptions {
    input: PathBuf,
    output: Option<PathBuf>,
    ocr: bool,
    ocr_lang: String,
}

fn main() -> ExitCode {
    match parse_args(std::env::args_os().skip(1)) {
        Ok(Some(options)) => match convert(&options.input, options.ocr, &options.ocr_lang) {
            Ok(markdown) => match write_output(options.output.as_deref(), markdown.as_bytes()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => fail(&format!("write failed: {error}")),
            },
            Err(error) => fail(&error),
        },
        Ok(None) => ExitCode::SUCCESS,
        Err(error) => fail(&error),
    }
}

fn parse_args(args: impl Iterator<Item = OsString>) -> Result<Option<CliOptions>, String> {
    let args: Vec<OsString> = args.collect();
    let mut input = None;
    let mut output = None;
    let mut ocr = true;
    let mut ocr_lang = std::env::var("UPARSER_OCR_LANG").unwrap_or_else(|_| "eng".to_owned());
    let mut index = 0;
    while index < args.len() {
        let argument = args[index].to_string_lossy();
        match argument.as_ref() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(None);
            }
            "parse" | "--no-assets" | "--no-cache" => {}
            "--ocr" => ocr = true,
            "--no-ocr" => ocr = false,
            "--ocr-lang" => {
                index += 1;
                ocr_lang = args
                    .get(index)
                    .ok_or_else(|| "--ocr-lang requires a value".to_owned())?
                    .to_string_lossy()
                    .into_owned();
            }
            "-o" | "--output" => {
                index += 1;
                output = Some(PathBuf::from(
                    args.get(index)
                        .ok_or_else(|| format!("{argument} requires a path"))?,
                ));
            }
            "--format" | "--protocol" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| format!("{argument} requires a value"))?
                    .to_string_lossy();
                let expected = if argument == "--format" {
                    "markdown"
                } else {
                    "native"
                };
                if value != expected {
                    return Err(format!("unsupported {argument} value: {value}"));
                }
            }
            value if value.starts_with('-') => return Err(format!("unknown option: {value}")),
            _ if input.is_some() => return Err(format!("unexpected argument: {argument}")),
            _ => input = Some(PathBuf::from(&args[index])),
        }
        index += 1;
    }
    input
        .map(|input| {
            Some(CliOptions {
                input,
                output,
                ocr,
                ocr_lang,
            })
        })
        .ok_or_else(|| USAGE.to_owned())
}

fn convert(input: &Path, ocr: bool, ocr_lang: &str) -> Result<String, String> {
    #[cfg(not(feature = "pdfium"))]
    let _ = ocr_lang;
    let pdf_hint = input
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"));
    let bytes = if pdf_hint {
        Vec::new()
    } else {
        std::fs::read(input).map_err(|error| format!("read failed: {error}"))?
    };
    let format = if pdf_hint {
        uparser_document_engine::DocumentFormat::Pdf
    } else {
        uparser_document_engine::detect_format(&bytes, input.to_str())
    };
    if format == uparser_document_engine::DocumentFormat::Pdf {
        let artifact = uparser_native_engine::process_pdf(input)
            .map_err(|error| format!("PDF parse failed: {error}"))?;
        if let Some(markdown) = artifact.markdown {
            return Ok(markdown);
        }
        #[cfg(feature = "pdfium")]
        if ocr {
            match tesseract_pdf(input, ocr_lang) {
                Ok(markdown) if !markdown.trim().is_empty() => return Ok(markdown),
                Ok(_) => eprintln!("warning: local OCR returned no text; using PDF text fallback"),
                Err(error) => {
                    eprintln!("warning: local OCR unavailable: {error}; using PDF text fallback")
                }
            }
        }
        #[cfg(not(feature = "pdfium"))]
        if ocr {
            eprintln!("warning: local OCR requires the pdfium feature; using PDF text fallback");
        }
        if artifact.positioned_items.is_empty() {
            let title = artifact.title.filter(|title| !title.trim().is_empty());
            return Ok(match title {
                Some(title) => format!("# {}\n\n[Image-only PDF: OCR required]\n", title.trim()),
                None => "[Image-only PDF: OCR required]\n".to_owned(),
            });
        }
        return Ok(uparser_native_engine::to_markdown_from_items(
            artifact.positioned_items,
            uparser_native_engine::MarkdownOptions::default(),
        ));
    }
    let options = uparser_document_engine::ParseOptions {
        include_assets: false,
        ..uparser_document_engine::ParseOptions::default()
    };
    let document = uparser_document_engine::parse_document(&bytes, format, &options)
        .map_err(|error| format!("document parse failed: {error}"))?;
    Ok(uparser_document_engine::render::markdown(&document))
}

#[cfg(feature = "pdfium")]
fn tesseract_pdf(input: &Path, language: &str) -> Result<String, String> {
    let tesseract = find_tesseract();
    let library = std::panic::catch_unwind(pdfium::Library::init)
        .map_err(|_| "failed to load pdfium shared library".to_owned())?;
    let document = library
        .load_document(input.to_string_lossy().as_ref(), None)
        .map_err(|error| format!("PDF rasterization failed: {error}"))?;
    let mut pages = Vec::with_capacity(document.page_count().max(0) as usize);
    for page_index in 0..document.page_count() {
        let page = document
            .page(page_index)
            .map_err(|error| format!("page {} load failed: {error}", page_index + 1))?;
        let bitmap = page
            .render(300.0)
            .map_err(|error| format!("page {} render failed: {error}", page_index + 1))?;
        let width = bitmap.width() as u32;
        let height = bitmap.height() as u32;
        let image =
            image::ImageBuffer::<image::Rgba<u8>, _>::from_raw(width, height, bitmap.to_rgba())
                .ok_or_else(|| {
                    format!("page {} returned an invalid RGBA buffer", page_index + 1)
                })?;
        let mut png = Vec::new();
        ocr_grayscale(image)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .map_err(|error| format!("page {} PNG encoding failed: {error}", page_index + 1))?;
        let mut source = tempfile::Builder::new()
            .suffix(".png")
            .tempfile()
            .map_err(|error| format!("temporary image creation failed: {error}"))?;
        source
            .write_all(&png)
            .map_err(|error| format!("temporary image write failed: {error}"))?;
        let mut command = std::process::Command::new(&tesseract);
        command.args([
            source.path().as_os_str(),
            std::ffi::OsStr::new("stdout"),
            std::ffi::OsStr::new("-l"),
            std::ffi::OsStr::new(language),
            std::ffi::OsStr::new("--oem"),
            std::ffi::OsStr::new("1"),
            std::ffi::OsStr::new("--psm"),
            std::ffi::OsStr::new("3"),
        ]);
        if let Some(tessdata) = tesseract.parent().map(|parent| parent.join("tessdata"))
            && tessdata.is_dir()
        {
            command.env("TESSDATA_PREFIX", tessdata);
        }
        let output = command
            .output()
            .map_err(|error| format!("{} failed to start: {error}", tesseract.display()))?;
        if !output.status.success() {
            return Err(format!(
                "Tesseract failed on page {}: {}",
                page_index + 1,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        pages.push(String::from_utf8_lossy(&output.stdout).trim().to_owned());
    }
    Ok(pages.join("\n\n"))
}

#[cfg(feature = "pdfium")]
fn find_tesseract() -> PathBuf {
    if let Some(path) = std::env::var_os("UPARSER_TESSERACT_PATH") {
        return PathBuf::from(path);
    }
    if let Ok(executable) = std::env::current_exe() {
        for ancestor in executable.ancestors() {
            let bundled = ancestor
                .join("tools")
                .join("tesseract")
                .join("tesseract.exe");
            if bundled.is_file() {
                return bundled;
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
            return installed;
        }
    }
    PathBuf::from("tesseract")
}

#[cfg(feature = "pdfium")]
fn ocr_grayscale(image: image::RgbaImage) -> image::DynamicImage {
    let grayscale = image::GrayImage::from_fn(image.width(), image.height(), |x, y| {
        let pixel = image.get_pixel(x, y);
        let alpha = u16::from(pixel[3]);
        let darkest = u16::from(pixel[0].min(pixel[1]).min(pixel[2]));
        let blended = (darkest * alpha + 255 * (255 - alpha)) / 255;
        image::Luma([blended as u8])
    });
    image::DynamicImage::ImageLuma8(grayscale)
}

fn write_output(output: Option<&Path>, markdown: &[u8]) -> std::io::Result<()> {
    match output {
        Some(path) => std::fs::write(path, markdown),
        None => std::io::stdout().lock().write_all(markdown),
    }
}

fn fail(message: &str) -> ExitCode {
    eprintln!("error: {message}");
    ExitCode::FAILURE
}
