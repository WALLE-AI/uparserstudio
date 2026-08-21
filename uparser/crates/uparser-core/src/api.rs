//! Library-level `parse`/`classify` entry points (T-10.1/T-10.2), shared
//! by the `uparser-napi`/`uparser-python` language bindings so all three
//! surfaces (CLI, Node, Python) call into the same core logic and
//! produce byte-identical IR, per `ARCHITECTURE.md`'s binding-layer
//! requirement.
//!
//! CLI and API are thin shells over the shared runner.

use crate::adapters::PipelineConfig;
#[cfg(test)]
use crate::types::RoutedBy;
use crate::types::{DocumentProfile, ParseResult};
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct ParseOptions {
    pub protocol: String,
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub window_size: usize,
    pub max_concurrency: usize,
    pub pipeline_config: PipelineConfig,
    pub no_cache: bool,
    /// Skip `postprocess::merge_paragraphs_by_geometry` and return each
    /// adapter's raw per-block output unmerged. Exists mainly so a
    /// caller can diff "raw protocol output" vs "post-processed output"
    /// when debugging a merge decision — off by default so `parse()`'s
    /// result is the fully post-processed one.
    pub no_postprocess: bool,
    /// Only parse these 1-indexed page numbers (see `page_range.rs`).
    /// `None` parses every page. Applied after rasterization/ingestion
    /// but before dispatching to the scheduler — lets a caller validate
    /// a protocol/endpoint against one page of a large document without
    /// waiting for every earlier page first.
    pub pages: Option<Vec<u32>>,
    /// Directory image/chart-category block crops get written to (see
    /// `assets.rs`), overriding `assets::default_assets_dir(path)`.
    /// Ignored when `no_assets` is set.
    pub assets_dir: Option<String>,
    /// Skip writing image assets to disk entirely (and leave every
    /// block's `asset_path` unset) — an explicit opt-out for a caller
    /// that doesn't want the filesystem side effect `write_page_assets`
    /// introduces by default (see `image_link_gap_report.md` for why
    /// that default-on behavior was chosen: it mirrors MinerU's own
    /// unconditional `images/` output convention).
    pub no_assets: bool,
    /// Shared across preflight analysis, optional L3 classification,
    /// conversion, page production and model dispatch.
    pub cancellation: crate::frontend::CancellationToken,
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self {
            protocol: "auto".to_string(),
            endpoint: None,
            model: None,
            // See cli.rs's `--window-size`/`--max-concurrency` doc
            // comments for the rationale behind these defaults: 64 keeps
            // any <=64-page document as a single barrier-free window, and
            // 16 is the measured sweet spot against a remote vLLM backend
            // (the old default of 4 badly under-fed the endpoint).
            window_size: 64,
            max_concurrency: 16,
            pipeline_config: PipelineConfig::default(),
            no_cache: false,
            no_postprocess: false,
            pages: None,
            assets_dir: None,
            no_assets: false,
            cancellation: crate::frontend::CancellationToken::default(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("no such file: {0}")]
    FileNotFound(String),
    #[error("failed to read file: {0}")]
    ReadFailed(String),
    #[error("unknown protocol: {0}")]
    UnknownProtocol(String),
    #[error("ingestion failed: {0}")]
    IngestFailed(String),
    #[error("preflight failed: {0}")]
    PreflightFailed(String),
    #[error("native parse failed: {0}")]
    NativeParseFailed(String),
    #[cfg(not(feature = "native"))]
    #[error("the `native` protocol requires building with the `native` feature")]
    NativeFeatureDisabled,
}

fn map_prepare_error(error: crate::runner::PrepareError) -> ApiError {
    match error {
        crate::runner::PrepareError::UnknownProtocol(protocol) => {
            ApiError::UnknownProtocol(protocol)
        }
        other => ApiError::PreflightFailed(other.to_string()),
    }
}

fn map_execution_error(error: crate::runner::ExecutionError) -> ApiError {
    match error {
        crate::runner::ExecutionError::UnknownProtocol(protocol) => {
            ApiError::UnknownProtocol(protocol)
        }
        crate::runner::ExecutionError::Ingest(message) => ApiError::IngestFailed(message),
        crate::runner::ExecutionError::Native(message)
        | crate::runner::ExecutionError::Structured(message) => {
            ApiError::NativeParseFailed(message)
        }
        crate::runner::ExecutionError::Assets(message) => ApiError::IngestFailed(message),
        crate::runner::ExecutionError::Cache(message) => ApiError::IngestFailed(message),
        crate::runner::ExecutionError::InvalidStageGraph(message) => {
            ApiError::PreflightFailed(message)
        }
        crate::runner::ExecutionError::Cancelled => {
            ApiError::PreflightFailed("execution cancelled".to_owned())
        }
    }
}

/// Runs the Profiler (L1, or L2 when `--features native` and the input
/// is a PDF) and returns the resulting `DocumentProfile`.
pub async fn classify(path: &str) -> Result<DocumentProfile, ApiError> {
    classify_with_cancellation(path, crate::frontend::CancellationToken::default()).await
}

pub async fn classify_with_cancellation(
    path: &str,
    cancellation: crate::frontend::CancellationToken,
) -> Result<DocumentProfile, ApiError> {
    if !Path::new(path).exists() {
        return Err(ApiError::FileNotFound(path.to_string()));
    }
    let bytes = std::fs::read(path).map_err(|e| ApiError::ReadFailed(e.to_string()))?;
    let source = crate::frontend::PreflightSource::new(Arc::<[u8]>::from(bytes), Some(path));
    crate::runner::analyze_with_cancellation(&source, &cancellation)
        .await
        .map(|report| report.profile)
        .map_err(|error| ApiError::PreflightFailed(error.to_string()))
}

/// Parse a structured source document into the lossless canonical contract.
/// PDF remains available through `parse`, whose page-oriented result carries
/// geometry that is not represented by the structured document engine.
pub async fn parse_canonical_document(
    path: &str,
    options: &uparser_document_engine::ParseOptions,
) -> Result<uparser_document_engine::CanonicalDocument, ApiError> {
    if !Path::new(path).exists() {
        return Err(ApiError::FileNotFound(path.to_owned()));
    }
    let bytes = std::fs::read(path).map_err(|error| ApiError::ReadFailed(error.to_string()))?;
    let format = uparser_document_engine::detect_format(&bytes, Some(path));
    if format == uparser_document_engine::DocumentFormat::Pdf {
        return Err(ApiError::NativeParseFailed(
            "canonical document output is currently limited to structured source formats"
                .to_owned(),
        ));
    }
    uparser_document_engine::parse_document(&bytes, format, options)
        .map_err(|error| ApiError::NativeParseFailed(error.to_string()))
}

/// Parses `path` with the given `options`, returning the full
/// `ParseResult` (page-level failures are carried in `page_errors`, not
/// surfaced as an `Err` — only whole-document failures are).
pub async fn parse(path: &str, options: &ParseOptions) -> Result<ParseResult, ApiError> {
    if !Path::new(path).exists() {
        return Err(ApiError::FileNotFound(path.to_string()));
    }
    let file_bytes = std::fs::read(path).map_err(|e| ApiError::ReadFailed(e.to_string()))?;
    let source = crate::frontend::PreflightSource::new(Arc::<[u8]>::from(file_bytes), Some(path));
    let prepared = crate::runner::prepare_with_preference_and_cancellation(
        source,
        Some(&options.protocol),
        crate::router::RoutePreference::Quality,
        options.cancellation.clone(),
    )
    .await
    .map_err(map_prepare_error)?;
    let execution = crate::runner::ExecutionOptions {
        endpoint: options.endpoint.clone(),
        model: options.model.clone(),
        window_size: options.window_size,
        max_concurrency: options.max_concurrency,
        pipeline_config: options.pipeline_config.clone(),
        no_cache: options.no_cache,
        no_postprocess: options.no_postprocess,
        pages: options.pages.clone(),
        assets_dir: options.assets_dir.as_ref().map(std::path::PathBuf::from),
        no_assets: options.no_assets,
        document_options: uparser_document_engine::ParseOptions::default(),
        cancellation: options.cancellation.clone(),
    };
    crate::runner::execute(prepared, &execution)
        .await
        .map(|outcome| outcome.result)
        .map_err(map_execution_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_png() -> Vec<u8> {
        let image = image::RgbImage::from_pixel(8, 8, image::Rgb([255, 255, 255]));
        let mut bytes = Vec::new();
        image
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    }

    #[tokio::test]
    async fn parse_mock_protocol_succeeds() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut file, &fixture_png()).unwrap();

        let options = ParseOptions {
            protocol: "mock".to_string(),
            no_cache: true,
            ..Default::default()
        };
        let result = parse(file.path().to_str().unwrap(), &options)
            .await
            .expect("mock parse succeeds");
        assert_eq!(result.protocol, "mock");
        assert_eq!(result.pages.len(), 1);
        assert!(result.page_errors.is_empty());
        assert_eq!(result.route_decision.as_ref().unwrap().protocol, "mock");
        assert_eq!(
            result.preprocess_plan.as_ref().unwrap().input_channel,
            crate::runner::InputChannel::VisualPages
        );
    }

    /// Source-semantic structured parsing is baseline capability and must not
    /// change when the PDF native feature is disabled.
    #[cfg(not(feature = "native"))]
    #[tokio::test]
    async fn parse_auto_routes_csv_to_baseline_document_engine() {
        let mut file = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
        std::io::Write::write_all(&mut file, b"Name,Age\nAlice,30\n").unwrap();

        let options = ParseOptions {
            protocol: "auto".to_string(),
            no_cache: true,
            ..Default::default()
        };
        let result = parse(file.path().to_str().unwrap(), &options)
            .await
            .expect("csv structured bypass succeeds");
        assert_eq!(result.protocol, "native:csv");
        assert_eq!(result.routed_by, RoutedBy::Auto);
        assert_eq!(
            result.document_profile.as_ref().unwrap().genre.primary,
            crate::types::DocumentGenre::Spreadsheet
        );
        assert_eq!(result.pages.len(), 1);
        let html = result.pages[0].blocks[0]
            .html
            .as_ref()
            .expect("csv produces an html table block");
        assert!(html.contains("Alice"));
    }

    #[cfg(feature = "native")]
    #[tokio::test]
    async fn parse_auto_routes_csv_to_native_document_engine() {
        let mut file = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
        std::io::Write::write_all(&mut file, b"Name,Age\nAlice,30\n").unwrap();
        let options = ParseOptions {
            protocol: "auto".to_owned(),
            no_cache: true,
            ..Default::default()
        };
        let result = parse(file.path().to_str().unwrap(), &options)
            .await
            .expect("CSV native parse succeeds");
        assert_eq!(result.protocol, "native:csv");
        assert_eq!(result.routed_by, RoutedBy::Auto);
        assert_eq!(
            result.pages[0].blocks[0].source,
            crate::types::BlockSource::StructuredNative
        );
        // A CSV lowers to a single table block, which carries its content as
        // HTML so merged cells survive; `text` is deliberately unset.
        assert!(
            result.pages[0].blocks[0]
                .html
                .as_deref()
                .unwrap()
                .contains("Alice")
        );
    }

    #[cfg(feature = "native")]
    #[tokio::test]
    async fn canonical_api_returns_lossless_structured_document() {
        let mut file = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
        std::io::Write::write_all(&mut file, b"Name,Age\nAlice,30\n").unwrap();
        let document = parse_canonical_document(
            file.path().to_str().unwrap(),
            &uparser_document_engine::ParseOptions::default(),
        )
        .await
        .expect("canonical CSV parse succeeds");
        assert_eq!(document.schema_version, "uparser.document.v1");
        assert_eq!(
            document.units[0].kind,
            uparser_document_engine::UnitKind::Sheet
        );
    }

    /// Proves DOCX input reaches real `normalize_format` conversion logic
    /// (previously: silently fell to the 1x1 placeholder and got fed to
    /// a protocol adapter as a blank image). LibreOffice isn't installed
    /// in this sandbox, so the expected outcome is a clean
    /// `IngestFailed` error, not a panic or silent wrong result — same
    /// "no tool available" gap already documented for `normalize_format`
    /// itself, now proven to surface through the real `parse()` call path.
    /// A minimal real ZIP archive containing a `word/` entry — the
    /// `file-format` crate's OOXML sniffing (confirmed from its source:
    /// `readers.rs` classifies any zip with an entry whose name starts
    /// with `word/` as `OfficeOpenXmlDocument`) needs a genuinely
    /// parseable zip, not just a `PK` magic-byte prefix.
    fn minimal_docx_bytes() -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            writer.start_file("[Content_Types].xml", options).unwrap();
            std::io::Write::write_all(
                &mut writer,
                b"<Types><Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/></Types>",
            )
            .unwrap();
            writer.start_file("_rels/.rels", options).unwrap();
            std::io::Write::write_all(
                &mut writer,
                b"<Relationships><Relationship Id=\"r0\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>",
            )
            .unwrap();
            writer.start_file("word/document.xml", options).unwrap();
            std::io::Write::write_all(
                &mut writer,
                b"<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body><w:p><w:r><w:t>hello</w:t></w:r></w:p></w:body></w:document>",
            )
            .unwrap();
            writer.finish().unwrap();
        }
        buf
    }

    #[tokio::test]
    async fn parse_docx_without_libreoffice_installed_is_a_clean_ingest_error() {
        let mut file = tempfile::Builder::new().suffix(".docx").tempfile().unwrap();
        std::io::Write::write_all(&mut file, &minimal_docx_bytes()).unwrap();

        let options = ParseOptions {
            protocol: "mock".to_string(),
            no_cache: true,
            ..Default::default()
        };
        let result = parse(file.path().to_str().unwrap(), &options).await;
        match result {
            Err(ApiError::IngestFailed(_)) => {}
            other => panic!("expected IngestFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn parse_pages_filter_excludes_pages_not_in_the_requested_set() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut file, &fixture_png()).unwrap();

        let options = ParseOptions {
            protocol: "mock".to_string(),
            no_cache: true,
            // mock/the non-pdfium fallback only ever produces page 1.
            pages: Some(vec![999]),
            ..Default::default()
        };
        let result = parse(file.path().to_str().unwrap(), &options)
            .await
            .expect("parse succeeds even if the page filter excludes everything");
        assert!(result.pages.is_empty());
    }

    #[tokio::test]
    async fn parse_pages_filter_keeps_matching_pages() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut file, &fixture_png()).unwrap();

        let options = ParseOptions {
            protocol: "mock".to_string(),
            no_cache: true,
            pages: Some(vec![1]),
            ..Default::default()
        };
        let result = parse(file.path().to_str().unwrap(), &options)
            .await
            .expect("parse succeeds");
        assert_eq!(result.pages.len(), 1);
    }

    /// Proves `postprocess::merge_paragraphs_by_geometry` is genuinely
    /// wired into the real `parse()` call path (T-9.1-era gap: it was
    /// only ever exercised by its own unit tests before this) — the
    /// mock adapter deliberately emits 2 raw, geometrically-mergeable
    /// blocks per page (see `adapters/mock.rs`'s doc comment) precisely
    /// so this can be verified end-to-end rather than by calling
    /// `postprocess::merge_paragraphs_by_geometry` directly.
    #[tokio::test]
    async fn parse_applies_postprocess_by_default() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut file, &fixture_png()).unwrap();

        let options = ParseOptions {
            protocol: "mock".to_string(),
            no_cache: true,
            ..Default::default()
        };
        let result = parse(file.path().to_str().unwrap(), &options)
            .await
            .expect("mock parse succeeds");
        assert_eq!(
            result.pages[0].blocks.len(),
            1,
            "mock's 2 raw blocks should have been merged into 1"
        );
    }

    #[tokio::test]
    async fn parse_no_postprocess_returns_raw_unmerged_blocks() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut file, &fixture_png()).unwrap();

        let options = ParseOptions {
            protocol: "mock".to_string(),
            no_cache: true,
            no_postprocess: true,
            ..Default::default()
        };
        let result = parse(file.path().to_str().unwrap(), &options)
            .await
            .expect("mock parse succeeds");
        assert_eq!(
            result.pages[0].blocks.len(),
            2,
            "--no-postprocess-equivalent option should return mock's raw 2 blocks"
        );
    }

    #[tokio::test]
    async fn parse_nonexistent_file_is_file_not_found() {
        let options = ParseOptions::default();
        let result = parse("/no/such/file.pdf", &options).await;
        assert!(matches!(result, Err(ApiError::FileNotFound(_))));
    }

    #[tokio::test]
    async fn parse_uses_the_same_cancellation_token_during_preflight() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut file, &fixture_png()).unwrap();
        let cancellation = crate::frontend::CancellationToken::default();
        cancellation.cancel();
        let options = ParseOptions {
            protocol: "mock".to_owned(),
            cancellation,
            ..Default::default()
        };

        let error = parse(file.path().to_str().unwrap(), &options)
            .await
            .unwrap_err();
        assert!(
            matches!(error, ApiError::PreflightFailed(message) if message.contains("cancelled"))
        );
    }

    #[tokio::test]
    async fn parse_unknown_protocol_is_unknown_protocol_error() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut file, &fixture_png()).unwrap();

        let options = ParseOptions {
            protocol: "nonexistent".to_string(),
            no_cache: true,
            ..Default::default()
        };
        let result = parse(file.path().to_str().unwrap(), &options).await;
        assert!(matches!(result, Err(ApiError::UnknownProtocol(_))));
    }

    #[tokio::test]
    async fn parse_respects_no_cache_and_cache_hit() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut file, &fixture_png()).unwrap();
        let cache_dir = tempfile::tempdir().unwrap();
        // SAFETY: test-only env var set for the duration of this test,
        // no concurrent access to it from other threads in this test.
        unsafe {
            std::env::set_var("UPARSER_CACHE_DIR", cache_dir.path());
        }

        let options = ParseOptions {
            protocol: "mock".to_string(),
            no_cache: false,
            ..Default::default()
        };
        let first = parse(file.path().to_str().unwrap(), &options)
            .await
            .unwrap();
        let second = parse(file.path().to_str().unwrap(), &options)
            .await
            .unwrap();
        assert_eq!(first, second);

        unsafe {
            std::env::remove_var("UPARSER_CACHE_DIR");
        }
    }

    #[tokio::test]
    async fn classify_produces_a_profile_for_a_real_file() {
        let mut file = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
        std::io::Write::write_all(&mut file, b"Name,Age\nAlice,30\n").unwrap();

        let profile = classify(file.path().to_str().unwrap()).await.unwrap();
        assert_eq!(
            profile.genre.primary,
            crate::types::DocumentGenre::Spreadsheet
        );
    }

    #[tokio::test]
    async fn classify_nonexistent_file_is_file_not_found() {
        let result = classify("/no/such/file.pdf").await;
        assert!(matches!(result, Err(ApiError::FileNotFound(_))));
    }
}
