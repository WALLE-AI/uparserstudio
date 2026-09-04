//! Document ingestion primitives used by the runner: PDF rasterization
//! (`rasterize*`, PDFium-gated) and external-tool format normalization
//! (`normalize_format`, LibreOffice/ImageMagick).
//!
//! Format detection lives in `frontend.rs` (the single authoritative
//! detection point) and structured-document reading lives in the
//! `uparser-document-engine` crate — the P7-era `detect_format`
//! wrapper, `structured_bypass` and `ingest_document` that used to sit
//! here were superseded by those two and removed in O1.

use std::time::Duration;
use thiserror::Error;

/// The structured-document engine owns format identity and detection. Core
/// re-exports that contract so every entry point carries the same 16 variants.
pub use crate::frontend::DocumentFormat;

/// A single rasterized page: PNG bytes plus pixel dimensions. Mirrors
/// liteparse's `RenderedPage` shape.
#[derive(Debug, Clone)]
pub struct RenderedPage {
    pub page_num: u32,
    pub width: u32,
    pub height: u32,
    pub png_bytes: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum IngestError {
    #[error("pdfium support was not compiled in (build with the `pdfium` feature)")]
    PdfiumFeatureDisabled,
    #[error("failed to rasterize PDF: {0}")]
    Rasterize(String),
    #[error("failed to read structured spreadsheet data: {0}")]
    StructuredParse(String),
    #[error("required conversion tool {tool:?} was not found on PATH")]
    ToolNotFound { tool: &'static str },
    #[error("conversion via {tool:?} failed: {message}")]
    ConversionFailed { tool: &'static str, message: String },
    #[error("conversion via {tool:?} timed out after {timeout:?}")]
    ConversionTimedOut {
        tool: &'static str,
        timeout: Duration,
    },
    #[error("format {0:?} is not supported for normalize_format")]
    UnsupportedFormat(DocumentFormat),
    #[error("failed to compute document profile: {0}")]
    Profiling(String),
}

/// External conversion tool binary names, overridable for testing (point
/// at a deliberately bogus command to exercise the `ToolNotFound` path
/// without depending on environment state).
#[derive(Clone)]
pub struct ToolNames {
    pub libreoffice: &'static str,
    pub imagemagick: &'static str,
}

impl Default for ToolNames {
    fn default() -> Self {
        Self {
            libreoffice: "soffice",
            imagemagick: "magick",
        }
    }
}

const DEFAULT_CONVERSION_TIMEOUT: Duration = Duration::from_secs(60);

/// Convert a non-PDF document to PDF via an external tool
/// (LibreOffice for DOCX/PPTX, ImageMagick for images). `Pdf` passes
/// through unchanged; `Xlsx`/`Csv` never reach this function in
/// `ingest_document`'s real control flow (they're bypassed earlier) but
/// pass through unchanged too, for robustness if called directly.
///
/// **T-7.5 evaluation** (LibreOffice/ImageMagick aren't installed in
/// this dev environment, so this documents the failure-handling design
/// actually implemented, not measured real-world timing):
/// - Degradation strategy: a bounded `tokio::time::timeout` wraps the
///   subprocess call, distinguishing `ConversionTimedOut` (the tool may
///   just be slow on a large/complex input — potentially worth a longer
///   budget) from `ToolNotFound` (a hard environment problem, never
///   worth retrying) and `ConversionFailed` (the tool ran and rejected
///   the input — also not retry-worthy without changing something). An
///   Agent consuming this should treat these three differently.
/// - Known industry-reported characteristic: headless LibreOffice
///   conversion is multi-second per document (not sub-second), scaling
///   with size/complexity/embedded fonts — relevant for choosing a
///   default timeout (60s here) generous enough for real documents
///   rather than tuned against synthetic ones.
/// - Real timing/failure-rate measurement against the actual binaries is
///   deferred until they're available in a test environment.
pub async fn normalize_format(
    bytes: &[u8],
    format: DocumentFormat,
) -> Result<Vec<u8>, IngestError> {
    normalize_format_with(
        bytes,
        format,
        ToolNames::default(),
        DEFAULT_CONVERSION_TIMEOUT,
    )
    .await
}

pub async fn normalize_format_with(
    bytes: &[u8],
    format: DocumentFormat,
    tools: ToolNames,
    timeout: Duration,
) -> Result<Vec<u8>, IngestError> {
    match format {
        DocumentFormat::Pdf => Ok(bytes.to_vec()),
        DocumentFormat::Doc
        | DocumentFormat::Docx
        | DocumentFormat::Ppt
        | DocumentFormat::Pptx
        | DocumentFormat::Odt
        | DocumentFormat::Ods
        | DocumentFormat::Odp
        | DocumentFormat::Rtf
        | DocumentFormat::Epub
        | DocumentFormat::Excel
        | DocumentFormat::Csv
        | DocumentFormat::Tsv => {
            convert_via_libreoffice(bytes, format, tools.libreoffice, timeout).await
        }
        DocumentFormat::Png | DocumentFormat::Jpeg => {
            convert_via_imagemagick(bytes, format, tools.imagemagick, timeout).await
        }
        DocumentFormat::Unknown => Err(IngestError::UnsupportedFormat(format)),
    }
}

async fn convert_via_libreoffice(
    bytes: &[u8],
    format: DocumentFormat,
    tool: &'static str,
    timeout: Duration,
) -> Result<Vec<u8>, IngestError> {
    let ext = match format {
        DocumentFormat::Doc => "doc",
        DocumentFormat::Docx => "docx",
        DocumentFormat::Ppt => "ppt",
        DocumentFormat::Pptx => "pptx",
        DocumentFormat::Odt => "odt",
        DocumentFormat::Ods => "ods",
        DocumentFormat::Odp => "odp",
        DocumentFormat::Rtf => "rtf",
        DocumentFormat::Epub => "epub",
        DocumentFormat::Excel => "xlsx",
        DocumentFormat::Csv => "csv",
        DocumentFormat::Tsv => "tsv",
        _ => unreachable!("caller only routes office/document formats here"),
    };

    let dir = tempfile::tempdir().map_err(|e| conversion_failed(tool, e))?;
    let input_path = dir.path().join(format!("input.{ext}"));
    tokio::fs::write(&input_path, bytes)
        .await
        .map_err(|e| conversion_failed(tool, e))?;

    // A unique `-env:UserInstallation` profile dir per invocation: headless
    // LibreOffice otherwise serializes on a shared per-user profile lock,
    // so a second concurrent conversion (a realistic batch-processing
    // scenario) hangs or fails waiting on the first instance's lock —
    // surfacing as a misleading `ConversionTimedOut` unrelated to the
    // actual document being converted.
    let profile_dir = dir.path().join("profile");
    let mut cmd = tokio::process::Command::new(tool);
    cmd.arg("--headless")
        .arg("--convert-to")
        .arg("pdf")
        .arg("--outdir")
        .arg(dir.path())
        .arg(format!(
            "-env:UserInstallation=file://{}",
            profile_dir.display()
        ))
        .arg(&input_path);
    let output = run_with_timeout(cmd, tool, timeout).await?;
    if !output.status.success() {
        return Err(IngestError::ConversionFailed {
            tool,
            message: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }

    let output_path = dir.path().join("input.pdf");
    tokio::fs::read(&output_path)
        .await
        .map_err(|e| conversion_failed(tool, e))
}

async fn convert_via_imagemagick(
    bytes: &[u8],
    format: DocumentFormat,
    tool: &'static str,
    timeout: Duration,
) -> Result<Vec<u8>, IngestError> {
    let ext = match format {
        DocumentFormat::Png => "png",
        DocumentFormat::Jpeg => "jpg",
        _ => unreachable!("caller only routes Png/Jpeg here"),
    };

    let dir = tempfile::tempdir().map_err(|e| conversion_failed(tool, e))?;
    let input_path = dir.path().join(format!("input.{ext}"));
    tokio::fs::write(&input_path, bytes)
        .await
        .map_err(|e| conversion_failed(tool, e))?;
    let output_path = dir.path().join("output.pdf");

    let mut cmd = tokio::process::Command::new(tool);
    cmd.arg(&input_path).arg(&output_path);
    let output = run_with_timeout(cmd, tool, timeout).await?;
    if !output.status.success() {
        return Err(IngestError::ConversionFailed {
            tool,
            message: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }

    tokio::fs::read(&output_path)
        .await
        .map_err(|e| conversion_failed(tool, e))
}

fn conversion_failed(tool: &'static str, e: impl std::fmt::Display) -> IngestError {
    IngestError::ConversionFailed {
        tool,
        message: e.to_string(),
    }
}

async fn run_with_timeout(
    mut cmd: tokio::process::Command,
    tool: &'static str,
    timeout: Duration,
) -> Result<std::process::Output, IngestError> {
    // Without this, a timed-out conversion leaves the real `soffice`/
    // `magick` process running as an orphan: `tokio::time::timeout`
    // dropping the `cmd.output()` future only abandons *our* handle to
    // the child, it doesn't kill it by default. Under sustained load
    // (repeated timeouts) this leaks OS processes/memory/file
    // descriptors indefinitely. Centralized here (rather than set at
    // each call site) so no future caller of this helper can forget it.
    cmd.kill_on_drop(true);
    match tokio::time::timeout(timeout, cmd.output()).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(IngestError::ToolNotFound { tool })
        }
        Ok(Err(e)) => Err(conversion_failed(tool, e)),
        Err(_) => Err(IngestError::ConversionTimedOut { tool, timeout }),
    }
}

#[cfg(feature = "pdfium")]
pub fn rasterize(path: &str, dpi: f32) -> Result<Vec<RenderedPage>, IngestError> {
    use pdfium::Library;

    let lib = Library::init();
    let document = lib
        .load_document(path, None)
        .map_err(|e| IngestError::Rasterize(e.to_string()))?;

    let page_count = document.page_count();
    let mut pages = Vec::with_capacity(page_count as usize);
    for index in 0..page_count {
        let page = document
            .page(index)
            .map_err(|e| IngestError::Rasterize(e.to_string()))?;
        let bitmap = page
            .render(dpi)
            .map_err(|e| IngestError::Rasterize(e.to_string()))?;
        let width = bitmap.width() as u32;
        let height = bitmap.height() as u32;
        let rgba = bitmap.to_rgba();
        let png_bytes = encode_png(&rgba, width, height)?;

        pages.push(RenderedPage {
            page_num: (index + 1) as u32,
            width,
            height,
            png_bytes,
        });
    }

    Ok(pages)
}

#[cfg(feature = "pdfium")]
pub fn pdf_page_count(pdf_bytes: &[u8]) -> Result<u32, IngestError> {
    use pdfium::Library;

    let library = Library::init();
    let document = library
        .load_document_from_bytes(pdf_bytes, None)
        .map_err(|error| IngestError::Rasterize(error.to_string()))?;
    u32::try_from(document.page_count())
        .map_err(|_| IngestError::Rasterize("PDF reported a negative page count".into()))
}

#[cfg(feature = "pdfium")]
pub fn rasterize_pdf_page_numbers(
    pdf_bytes: &[u8],
    dpi: f32,
    page_numbers: &[u32],
) -> Result<Vec<RenderedPage>, IngestError> {
    use pdfium::Library;

    let library = Library::init();
    let document = library
        .load_document_from_bytes(pdf_bytes, None)
        .map_err(|error| IngestError::Rasterize(error.to_string()))?;
    let page_count = document.page_count();
    let mut pages = Vec::with_capacity(page_numbers.len());
    for page_num in page_numbers {
        if *page_num == 0 || i64::from(*page_num) > i64::from(page_count) {
            continue;
        }
        let page = document
            .page((*page_num - 1) as i32)
            .map_err(|error| IngestError::Rasterize(error.to_string()))?;
        let bitmap = page
            .render(dpi)
            .map_err(|error| IngestError::Rasterize(error.to_string()))?;
        let width = bitmap.width() as u32;
        let height = bitmap.height() as u32;
        let png_bytes = encode_png(&bitmap.to_rgba(), width, height)?;
        pages.push(RenderedPage {
            page_num: *page_num,
            width,
            height,
            png_bytes,
        });
    }
    Ok(pages)
}

#[cfg(feature = "pdfium")]
fn encode_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, IngestError> {
    use image::{ImageBuffer, Rgba};

    let buffer: ImageBuffer<Rgba<u8>, _> = ImageBuffer::from_raw(width, height, rgba.to_vec())
        .ok_or_else(|| IngestError::Rasterize("RGBA buffer size mismatch".into()))?;

    let mut out = Vec::new();
    buffer
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .map_err(|e| IngestError::Rasterize(e.to_string()))?;
    Ok(out)
}

#[cfg(not(feature = "pdfium"))]
pub fn rasterize(_path: &str, _dpi: f32) -> Result<Vec<RenderedPage>, IngestError> {
    Err(IngestError::PdfiumFeatureDisabled)
}

/// Rasterize already-in-memory PDF bytes (e.g. `normalize_format`'s
/// LibreOffice-converted output) — `rasterize()` only accepts a file
/// path (pdfium's `Library::load_document` reads from disk), so this
/// writes to a temp file first. Used by the DOCX/PPTX ingestion path,
/// where there's no original on-disk PDF to point `rasterize()` at.
pub fn rasterize_pdf_bytes(pdf_bytes: &[u8], dpi: f32) -> Result<Vec<RenderedPage>, IngestError> {
    #[cfg(feature = "pdfium")]
    {
        let mut tmp = tempfile::Builder::new()
            .suffix(".pdf")
            .tempfile()
            .map_err(|e| IngestError::Rasterize(format!("failed to create temp file: {e}")))?;
        std::io::Write::write_all(&mut tmp, pdf_bytes)
            .map_err(|e| IngestError::Rasterize(format!("failed to write temp file: {e}")))?;
        let path = tmp.path().to_str().ok_or_else(|| {
            IngestError::Rasterize("temp file path is not valid UTF-8".to_string())
        })?;
        rasterize(path, dpi)
    }
    #[cfg(not(feature = "pdfium"))]
    {
        let _ = (pdf_bytes, dpi);
        Err(IngestError::PdfiumFeatureDisabled)
    }
}

#[cfg(all(test, feature = "pdfium"))]
mod rasterize_tests {
    use super::*;

    #[test]
    fn rasterizes_fixture_pdf() {
        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../opensource/liteparse/integration_tests_data/sample.pdf"
        );
        if !std::path::Path::new(fixture).exists() {
            eprintln!("skipping: no fixture PDF at {fixture}");
            return;
        }
        let pages = rasterize(fixture, 100.0).expect("rasterize should succeed");
        assert!(!pages.is_empty());
        for page in &pages {
            assert!(page.width > 0 && page.height > 0);
            assert!(!page.png_bytes.is_empty());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- normalize_format ---

    fn bogus_tools() -> ToolNames {
        ToolNames {
            libreoffice: "definitely-not-a-real-binary-xyz",
            imagemagick: "definitely-not-a-real-binary-xyz",
        }
    }

    #[tokio::test]
    async fn normalize_format_pdf_passthrough() {
        let bytes = b"%PDF-1.7 fake content";
        let out = normalize_format(bytes, DocumentFormat::Pdf).await.unwrap();
        assert_eq!(out, bytes);
    }

    #[tokio::test]
    async fn normalize_format_missing_libreoffice_binary_is_tool_not_found() {
        let result = normalize_format_with(
            b"fake docx",
            DocumentFormat::Docx,
            bogus_tools(),
            Duration::from_secs(5),
        )
        .await;
        assert!(matches!(result, Err(IngestError::ToolNotFound { .. })));
    }

    #[tokio::test]
    async fn normalize_format_missing_imagemagick_binary_is_tool_not_found() {
        let result = normalize_format_with(
            b"fake png",
            DocumentFormat::Png,
            bogus_tools(),
            Duration::from_secs(5),
        )
        .await;
        assert!(matches!(result, Err(IngestError::ToolNotFound { .. })));
    }

    #[tokio::test]
    async fn normalize_format_unknown_format_is_unsupported() {
        let result = normalize_format(b"???", DocumentFormat::Unknown).await;
        assert!(matches!(
            result,
            Err(IngestError::UnsupportedFormat(DocumentFormat::Unknown))
        ));
    }

    #[tokio::test]
    async fn every_office_extension_routes_through_the_conversion_tool() {
        for format in [
            DocumentFormat::Doc,
            DocumentFormat::Docx,
            DocumentFormat::Ppt,
            DocumentFormat::Pptx,
            DocumentFormat::Odt,
            DocumentFormat::Ods,
            DocumentFormat::Odp,
            DocumentFormat::Rtf,
            DocumentFormat::Epub,
            DocumentFormat::Excel,
            DocumentFormat::Csv,
            DocumentFormat::Tsv,
        ] {
            let result =
                normalize_format_with(b"input", format, bogus_tools(), Duration::from_secs(1))
                    .await;
            assert!(matches!(result, Err(IngestError::ToolNotFound { .. })));
        }
        let jpeg = normalize_format_with(
            b"jpeg",
            DocumentFormat::Jpeg,
            bogus_tools(),
            Duration::from_secs(1),
        )
        .await;
        assert!(matches!(jpeg, Err(IngestError::ToolNotFound { .. })));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn converters_report_nonzero_tool_exits_and_spawn_failures() {
        let tools = ToolNames {
            libreoffice: "where.exe",
            imagemagick: "where.exe",
        };
        let office = normalize_format_with(
            b"doc",
            DocumentFormat::Doc,
            tools.clone(),
            Duration::from_secs(2),
        )
        .await;
        assert!(matches!(office, Err(IngestError::ConversionFailed { .. })));

        let image =
            normalize_format_with(b"png", DocumentFormat::Png, tools, Duration::from_secs(2)).await;
        assert!(matches!(image, Err(IngestError::ConversionFailed { .. })));

        let directory = tempfile::tempdir().unwrap();
        let cmd = tokio::process::Command::new(directory.path());
        let result = run_with_timeout(cmd, "directory", Duration::from_secs(1)).await;
        assert!(matches!(result, Err(IngestError::ConversionFailed { .. })));
    }

    #[cfg(not(feature = "pdfium"))]
    #[test]
    fn rasterization_without_pdfium_returns_the_feature_error() {
        assert!(matches!(
            rasterize("missing.pdf", 100.0),
            Err(IngestError::PdfiumFeatureDisabled)
        ));
        assert!(matches!(
            rasterize_pdf_bytes(b"%PDF-1.7", 100.0),
            Err(IngestError::PdfiumFeatureDisabled)
        ));
    }

    #[tokio::test]
    async fn run_with_timeout_reports_timeout_not_failure() {
        let cmd = long_running_command();
        let result = run_with_timeout(cmd, "test-delay", Duration::from_millis(100)).await;
        assert!(matches!(
            result,
            Err(IngestError::ConversionTimedOut { .. })
        ));
    }

    /// Confirms the exact mechanism `run_with_timeout`'s `kill_on_drop`
    /// fix relies on: dropping a timed-out child future for a
    /// `kill_on_drop(true)` process actually terminates the OS process,
    /// not just abandons tokio's handle to it. Before this fix, a timed-
    /// out `soffice`/`magick` conversion left the real subprocess running
    /// as an orphan indefinitely (confirmed by this same test failing —
    /// process still alive — when `kill_on_drop(true)` is removed).
    #[tokio::test]
    async fn kill_on_drop_actually_terminates_the_child_on_timeout() {
        let mut cmd = long_running_command();
        cmd.kill_on_drop(true);
        let mut child = cmd.spawn().expect("failed to spawn delay process");
        let pid = child.id().expect("spawned child has a pid");

        let result = tokio::time::timeout(Duration::from_millis(100), child.wait()).await;
        assert!(result.is_err(), "expected the wait to time out");
        drop(child);

        // Give the OS a brief moment to actually reap the process after
        // the kill signal fires.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let still_alive = process_exists(pid);
        assert!(
            !still_alive,
            "process {pid} should have been killed on drop"
        );
    }

    #[cfg(windows)]
    fn long_running_command() -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("cmd.exe");
        cmd.args(["/C", "ping -n 6 127.0.0.1 >NUL"]);
        cmd
    }

    #[cfg(not(windows))]
    fn long_running_command() -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("sleep");
        cmd.arg("5");
        cmd
    }

    #[cfg(windows)]
    fn process_exists(pid: u32) -> bool {
        let filter = format!("PID eq {pid}");
        std::process::Command::new("tasklist.exe")
            .args(["/FI", &filter, "/FO", "CSV", "/NH"])
            .output()
            .map(|output| {
                String::from_utf8_lossy(&output.stdout)
                    .split(',')
                    .nth(1)
                    .is_some_and(|field| field.trim_matches('"') == pid.to_string())
            })
            .unwrap_or(false)
    }

    #[cfg(not(windows))]
    fn process_exists(pid: u32) -> bool {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }
}
