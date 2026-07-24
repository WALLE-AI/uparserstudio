//! Document ingestion. P0 only implements `rasterize()`; P7 adds
//! `detect_format`, `structured_bypass`, and `normalize_format`, wired
//! together by `ingest_document` in the canonical order fixed by
//! ARCHITECTURE.md §13.1a: `detect_format → structured_bypass? →
//! normalize_format → rasterize` (the last step lives in `rasterize()`
//! itself, called separately downstream).

use crate::types::{Block, BlockSource, CoordFrame, Geometry, Page, ParseResult, RoutedBy};
use file_format::FileFormat;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Duration;
use thiserror::Error;

/// Detected input document format, before any conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentFormat {
    Pdf,
    Docx,
    Pptx,
    Xlsx,
    Csv,
    Png,
    Jpeg,
    Unknown,
}

/// Sniff the document format from content (magic bytes via the
/// `file-format` crate — the same dependency `opensource/liteparse`
/// already uses for this exact purpose). CSV has no reliable magic-byte
/// signature (it's plain text), so content sniffing alone can't
/// distinguish it from other plain text — fall back to the filename
/// extension hint in that case.
pub fn detect_format(bytes: &[u8], filename_hint: Option<&str>) -> DocumentFormat {
    let fmt = FileFormat::from_bytes(bytes);
    match fmt.extension() {
        "pdf" => DocumentFormat::Pdf,
        "docx" => DocumentFormat::Docx,
        "pptx" => DocumentFormat::Pptx,
        "xlsx" => DocumentFormat::Xlsx,
        "png" => DocumentFormat::Png,
        "jpg" => DocumentFormat::Jpeg,
        _ => {
            if let Some(name) = filename_hint {
                let lower = name.to_lowercase();
                if lower.ends_with(".csv") {
                    return DocumentFormat::Csv;
                }
            }
            DocumentFormat::Unknown
        }
    }
}

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

/// Structured-data short-circuit: XLSX/CSV are read directly as cell
/// grids — no rasterization, no model call at all. Returns `None` for
/// any other format (the "does this format even apply" gate in
/// `ingest_document`'s control flow), `Some(Ok(..))`/`Some(Err(..))`
/// otherwise.
pub fn structured_bypass(
    bytes: &[u8],
    format: DocumentFormat,
    source_path: &str,
) -> Option<Result<ParseResult, IngestError>> {
    match format {
        DocumentFormat::Xlsx => Some(structured_bypass_xlsx(bytes, source_path)),
        DocumentFormat::Csv => Some(structured_bypass_csv(bytes, source_path)),
        _ => None,
    }
}

fn structured_bypass_xlsx(bytes: &[u8], source_path: &str) -> Result<ParseResult, IngestError> {
    use calamine::Reader;

    let cursor = std::io::Cursor::new(bytes);
    let mut workbook =
        calamine::Xlsx::new(cursor).map_err(|e| IngestError::StructuredParse(e.to_string()))?;

    let pages: Vec<Page> = workbook
        .worksheets()
        .into_iter()
        .enumerate()
        .map(|(idx, (_name, range))| {
            let rows: Vec<Vec<String>> = range
                .rows()
                .map(|row| row.iter().map(|cell| cell.to_string()).collect())
                .collect();
            Page {
                page_num: (idx + 1) as u32,
                width_px: 0,
                height_px: 0,
                blocks: vec![sheet_block(&rows)],
            }
        })
        .collect();

    Ok(build_structured_result(source_path, bytes, pages, "xlsx"))
}

fn structured_bypass_csv(bytes: &[u8], source_path: &str) -> Result<ParseResult, IngestError> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .from_reader(bytes);
    let mut rows: Vec<Vec<String>> = Vec::new();
    for record in reader.records() {
        let record = record.map_err(|e| IngestError::StructuredParse(e.to_string()))?;
        rows.push(record.iter().map(|s| s.to_string()).collect());
    }

    let page = Page {
        page_num: 1,
        width_px: 0,
        height_px: 0,
        blocks: vec![sheet_block(&rows)],
    };

    Ok(build_structured_result(
        source_path,
        bytes,
        vec![page],
        "csv",
    ))
}

fn sheet_block(rows: &[Vec<String>]) -> Block {
    let mut html = String::from("<table>");
    for row in rows {
        html.push_str("<tr>");
        for cell in row {
            html.push_str("<td>");
            html.push_str(&crate::otsl::escape_html(cell));
            html.push_str("</td>");
        }
        html.push_str("</tr>");
    }
    html.push_str("</table>");

    Block {
        geom: Geometry::Rect([0.0, 0.0, 0.0, 0.0]),
        geom_frame: CoordFrame::Page,
        bbox_px: None,
        category_raw: "table".to_string(),
        category: Some("table".to_string()),
        reading_order: Some(0),
        text: None,
        html: Some(html),
        latex: None,
        spans: vec![],
        merge_hint: None,
        confidence: None,
        source: BlockSource::StructuredNative,
        error: None,
    }
}

fn build_structured_result(
    source_path: &str,
    bytes: &[u8],
    pages: Vec<Page>,
    kind: &str,
) -> ParseResult {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let source_sha256 = format!("{:x}", hasher.finalize());

    ParseResult {
        source_path: source_path.to_string(),
        source_sha256,
        protocol: format!("structured_bypass:{kind}"),
        routed_by: RoutedBy::Explicit,
        document_profile: None,
        model_endpoint: None,
        model_name: None,
        pages,
        page_errors: vec![],
        capability_notes: vec![],
        warnings: vec![],
        timing: Default::default(),
    }
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
        DocumentFormat::Pdf | DocumentFormat::Xlsx | DocumentFormat::Csv => Ok(bytes.to_vec()),
        DocumentFormat::Docx | DocumentFormat::Pptx => {
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
        DocumentFormat::Docx => "docx",
        DocumentFormat::Pptx => "pptx",
        _ => unreachable!("caller only routes Docx/Pptx here"),
    };

    let dir = tempfile::tempdir().map_err(|e| conversion_failed(tool, e))?;
    let input_path = dir.path().join(format!("input.{ext}"));
    tokio::fs::write(&input_path, bytes)
        .await
        .map_err(|e| conversion_failed(tool, e))?;

    let mut cmd = tokio::process::Command::new(tool);
    cmd.arg("--headless")
        .arg("--convert-to")
        .arg("pdf")
        .arg("--outdir")
        .arg(dir.path())
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
    match tokio::time::timeout(timeout, cmd.output()).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(IngestError::ToolNotFound { tool })
        }
        Ok(Err(e)) => Err(conversion_failed(tool, e)),
        Err(_) => Err(IngestError::ConversionTimedOut { tool, timeout }),
    }
}

/// Result of `ingest_document`: either a fully-formed structured result
/// (XLSX/CSV bypass — no further pipeline steps needed) or PDF bytes
/// ready for the next pipeline step (`rasterize()`, called separately).
#[derive(Debug)]
pub enum IngestOutcome {
    Structured(Box<ParseResult>),
    Pdf(Vec<u8>),
}

/// Ties `detect_format`/`structured_bypass`/`normalize_format` together
/// in the canonical order fixed by ARCHITECTURE.md §13.1a:
/// `detect_format → structured_bypass? → normalize_format → (rasterize,
/// called separately downstream)`.
pub async fn ingest_document(
    bytes: &[u8],
    source_path: &str,
) -> Result<IngestOutcome, IngestError> {
    let format = detect_format(bytes, Some(source_path));

    if let Some(result) = structured_bypass(bytes, format, source_path) {
        return result.map(|r| IngestOutcome::Structured(Box::new(r)));
    }

    let pdf_bytes = normalize_format(bytes, format).await?;
    Ok(IngestOutcome::Pdf(pdf_bytes))
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

    // --- detect_format ---

    #[test]
    fn detects_pdf_by_magic_bytes() {
        assert_eq!(detect_format(b"%PDF-1.7\n", None), DocumentFormat::Pdf);
    }

    #[test]
    fn detects_png_by_magic_bytes() {
        let png_sig: &[u8] = &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        assert_eq!(detect_format(png_sig, None), DocumentFormat::Png);
    }

    #[test]
    fn detects_jpeg_by_magic_bytes() {
        let jpeg_sig: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0];
        assert_eq!(detect_format(jpeg_sig, None), DocumentFormat::Jpeg);
    }

    #[test]
    fn csv_falls_back_to_extension_hint() {
        let csv_bytes = b"a,b,c\n1,2,3\n";
        assert_eq!(
            detect_format(csv_bytes, Some("data.csv")),
            DocumentFormat::Csv
        );
        // Without the hint, plain text has no distinguishing magic bytes.
        assert_eq!(detect_format(csv_bytes, None), DocumentFormat::Unknown);
    }

    #[test]
    fn unrecognized_bytes_are_unknown() {
        assert_eq!(
            detect_format(&[0, 1, 2, 3, 4], None),
            DocumentFormat::Unknown
        );
    }

    // --- structured_bypass ---

    #[test]
    fn structured_bypass_returns_none_for_non_spreadsheet_formats() {
        assert!(structured_bypass(b"%PDF-1.7", DocumentFormat::Pdf, "doc.pdf").is_none());
    }

    #[test]
    fn structured_bypass_csv_builds_table_block() {
        let csv_bytes = b"Name,Age\nAlice,30\nBob,25\n";
        let result = structured_bypass(csv_bytes, DocumentFormat::Csv, "people.csv")
            .expect("csv is bypassed")
            .expect("csv parses");

        assert_eq!(result.protocol, "structured_bypass:csv");
        assert_eq!(result.pages.len(), 1);
        let block = &result.pages[0].blocks[0];
        assert_eq!(block.source, BlockSource::StructuredNative);
        assert_eq!(block.category.as_deref(), Some("table"));
        let html = block.html.as_ref().expect("csv produces html");
        assert!(html.contains("<table>"));
        assert!(html.contains("Alice"));
        assert!(html.contains("30"));
    }

    #[test]
    fn structured_bypass_csv_escapes_html() {
        let csv_bytes = b"a,b\n<script>,\"x & y\"\n";
        let result = structured_bypass(csv_bytes, DocumentFormat::Csv, "x.csv")
            .unwrap()
            .unwrap();
        let html = result.pages[0].blocks[0].html.as_ref().unwrap();
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("x &amp; y"));
    }

    #[test]
    fn structured_bypass_malformed_csv_does_not_panic() {
        // Unterminated quote — csv crate should surface a parse error,
        // not panic.
        let malformed = b"a,\"b\n";
        let result = structured_bypass(malformed, DocumentFormat::Csv, "bad.csv").unwrap();
        // Either a structured error or (if the csv crate tolerates it) a
        // successful parse — the key assertion is "did not panic".
        let _ = result;
    }

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
    async fn run_with_timeout_reports_timeout_not_failure() {
        let mut cmd = tokio::process::Command::new("sleep");
        cmd.arg("5");
        let result = run_with_timeout(cmd, "sleep", Duration::from_millis(100)).await;
        assert!(matches!(
            result,
            Err(IngestError::ConversionTimedOut { .. })
        ));
    }

    // --- ingest_document ---

    #[tokio::test]
    async fn ingest_document_xlsx_shaped_input_short_circuits_to_structured() {
        let csv_bytes = b"a,b\n1,2\n";
        // Route via detect_format's extension hint by using a .csv path.
        let outcome = ingest_document(csv_bytes, "sheet.csv").await.unwrap();
        assert!(matches!(outcome, IngestOutcome::Structured(_)));
    }

    #[tokio::test]
    async fn ingest_document_pdf_passes_through_without_conversion() {
        let pdf_bytes = b"%PDF-1.7 fake";
        let outcome = ingest_document(pdf_bytes, "doc.pdf").await.unwrap();
        match outcome {
            IngestOutcome::Pdf(bytes) => assert_eq!(bytes, pdf_bytes),
            IngestOutcome::Structured(_) => panic!("PDF should not be structured-bypassed"),
        }
    }
}
