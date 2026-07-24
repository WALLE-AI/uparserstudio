//! Library-level `parse`/`classify` entry points (T-10.1/T-10.2), shared
//! by the `uparser-napi`/`uparser-python` language bindings so all three
//! surfaces (CLI, Node, Python) call into the same core logic and
//! produce byte-identical IR, per `ARCHITECTURE.md`'s binding-layer
//! requirement.
//!
//! **Known, documented duplication**: `cli.rs::run_parse` implements
//! largely the same logic (registry build, cache lookup/store, rasterize,
//! scheduler dispatch) independently, rather than this module being
//! extracted *from* it — `cli.rs` additionally handles `--stream`'s
//! incremental NDJSON output (a CLI-only UX concern the bindings don't
//! need for a single request/response call) and CLI-specific exit-code/
//! stderr-formatting concerns this module has no business doing. Fully
//! unifying them was judged higher-risk than valuable for this pass
//! (touching `cli.rs`'s already-tested control flow to shave off
//! duplication, versus adding this new, independently-tested module) —
//! left as a documented follow-up, not a hidden one.

use crate::adapters::{AdapterOverrides, PipelineConfig, Registry};
use crate::cache::{self, ParamFingerprint};
use crate::ingest::{DocumentFormat, RenderedPage};
use crate::scheduler::Scheduler;
use crate::transport::Transport;
use crate::types::{DocumentProfile, ParseResult, RoutedBy};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

/// Same default as `cli.rs`'s `DEFAULT_CACHE_TTL` (kept as an
/// independent constant per this module's documented-duplication
/// posture above).
pub const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone)]
pub struct ParseOptions {
    pub protocol: String,
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub window_size: usize,
    pub max_concurrency: usize,
    pub pipeline_config: PipelineConfig,
    pub no_cache: bool,
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self {
            protocol: "mock".to_string(),
            endpoint: None,
            model: None,
            window_size: 16,
            max_concurrency: 4,
            pipeline_config: PipelineConfig::default(),
            no_cache: false,
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
    #[cfg(feature = "native")]
    #[error("native parse failed: {0}")]
    NativeParseFailed(String),
    #[cfg(not(feature = "native"))]
    #[error("the `native` protocol requires building with the `native` feature")]
    NativeFeatureDisabled,
}

/// Same fallback ladder as `cli.rs::rasterize_or_fallback` — see that
/// function's doc comment for the real-bug history behind why the
/// non-`pdfium` fallback must decode real image bytes rather than
/// hardcoding `1x1`.
fn rasterize_or_fallback(path: &str, file_bytes: &[u8]) -> Vec<RenderedPage> {
    #[cfg(feature = "pdfium")]
    {
        if let Ok(pages) = crate::ingest::rasterize(path, 150.0)
            && !pages.is_empty()
        {
            return pages;
        }
    }
    #[cfg(not(feature = "pdfium"))]
    let _ = path;

    if let Ok(img) = image::load_from_memory(file_bytes) {
        return vec![RenderedPage {
            page_num: 1,
            width: img.width(),
            height: img.height(),
            png_bytes: file_bytes.to_vec(),
        }];
    }

    vec![RenderedPage {
        page_num: 1,
        width: 1,
        height: 1,
        png_bytes: file_bytes.to_vec(),
    }]
}

/// L2 profiling when possible (native feature + PDF input), falling back
/// to L1 otherwise — mirrors `cli.rs::profile_best_effort`.
async fn profile_best_effort(bytes: &[u8], format: DocumentFormat) -> DocumentProfile {
    #[cfg(feature = "native")]
    {
        if format == DocumentFormat::Pdf
            && let Ok(p) = crate::profiler::profile_l2(bytes, format).await
        {
            return p;
        }
    }
    #[cfg(not(feature = "native"))]
    let _ = &bytes;

    crate::profiler::profile_l1(format)
}

/// Runs the Profiler (L1, or L2 when `--features native` and the input
/// is a PDF) and returns the resulting `DocumentProfile`.
pub async fn classify(path: &str) -> Result<DocumentProfile, ApiError> {
    if !Path::new(path).exists() {
        return Err(ApiError::FileNotFound(path.to_string()));
    }
    let bytes = std::fs::read(path).map_err(|e| ApiError::ReadFailed(e.to_string()))?;
    let format = crate::ingest::detect_format(&bytes, Some(path));
    Ok(profile_best_effort(&bytes, format).await)
}

/// Parses `path` with the given `options`, returning the full
/// `ParseResult` (page-level failures are carried in `page_errors`, not
/// surfaced as an `Err` — only whole-document failures are).
pub async fn parse(path: &str, options: &ParseOptions) -> Result<ParseResult, ApiError> {
    if !Path::new(path).exists() {
        return Err(ApiError::FileNotFound(path.to_string()));
    }
    let file_bytes = std::fs::read(path).map_err(|e| ApiError::ReadFailed(e.to_string()))?;

    let effective_protocol = if options.protocol == "auto" {
        let format = crate::ingest::detect_format(&file_bytes, Some(path));
        let profile = profile_best_effort(&file_bytes, format).await;
        crate::router::route(&profile).protocol.to_string()
    } else {
        options.protocol.clone()
    };

    if effective_protocol == "native" {
        return parse_native(path, &file_bytes).await;
    }

    let fingerprint = ParamFingerprint {
        protocol: effective_protocol.clone(),
        endpoint: options.endpoint.clone(),
        model: options.model.clone(),
    };
    let cache_key = cache::cache_key(&file_bytes, &fingerprint);
    let cache_dir = cache::default_cache_dir();
    if !options.no_cache
        && let Some(cached) = cache::get(&cache_dir, &cache_key, DEFAULT_CACHE_TTL)
    {
        return Ok(cached);
    }

    let registry = Registry::with_builtins();
    let overrides = AdapterOverrides {
        endpoint: options.endpoint.clone(),
        model: options.model.clone(),
        pipeline: Some(options.pipeline_config.clone()),
    };
    let adapter = registry
        .build(&effective_protocol, &overrides)
        .ok_or_else(|| ApiError::UnknownProtocol(effective_protocol.clone()))?;

    let source_sha256 = {
        let mut hasher = Sha256::new();
        hasher.update(&file_bytes);
        format!("{:x}", hasher.finalize())
    };
    let pages = rasterize_or_fallback(path, &file_bytes);

    let transport = Arc::new(Transport::new());
    let permits = Arc::new(Semaphore::new(options.max_concurrency.max(1)));
    let scheduler = Scheduler::new(options.window_size.max(1));
    let (result_pages, page_errors) = scheduler.run(adapter, transport, permits, pages).await;

    let result = ParseResult {
        source_path: path.to_string(),
        source_sha256,
        protocol: effective_protocol,
        routed_by: if options.protocol == "auto" {
            RoutedBy::Auto
        } else {
            RoutedBy::Explicit
        },
        document_profile: None,
        model_endpoint: None,
        model_name: None,
        pages: result_pages,
        page_errors,
        capability_notes: vec![],
        warnings: vec![],
        timing: Default::default(),
    };

    if !options.no_cache {
        let _ = cache::put(&cache_dir, &cache_key, &result);
    }

    Ok(result)
}

#[cfg(feature = "native")]
async fn parse_native(path: &str, file_bytes: &[u8]) -> Result<ParseResult, ApiError> {
    let adapter = crate::adapters::native::NativeAdapter;
    adapter
        .parse_document(path, file_bytes)
        .await
        .map_err(|e| ApiError::NativeParseFailed(e.message))
}

#[cfg(not(feature = "native"))]
async fn parse_native(_path: &str, _file_bytes: &[u8]) -> Result<ParseResult, ApiError> {
    Err(ApiError::NativeFeatureDisabled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn parse_mock_protocol_succeeds() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut file, b"fake pdf bytes").unwrap();

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
    }

    #[tokio::test]
    async fn parse_nonexistent_file_is_file_not_found() {
        let options = ParseOptions::default();
        let result = parse("/no/such/file.pdf", &options).await;
        assert!(matches!(result, Err(ApiError::FileNotFound(_))));
    }

    #[tokio::test]
    async fn parse_unknown_protocol_is_unknown_protocol_error() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut file, b"fake pdf bytes").unwrap();

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
        std::io::Write::write_all(&mut file, b"fake pdf bytes for api cache test").unwrap();
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
        let mut file = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut file, b"not really a pdf, just bytes").unwrap();

        let profile = classify(file.path().to_str().unwrap()).await.unwrap();
        assert!(profile.kind_confidence >= 0.0);
    }

    #[tokio::test]
    async fn classify_nonexistent_file_is_file_not_found() {
        let result = classify("/no/such/file.pdf").await;
        assert!(matches!(result, Err(ApiError::FileNotFound(_))));
    }
}
