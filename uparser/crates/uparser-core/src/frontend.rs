//! Authoritative source detection and immutable input identity.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Notify;

pub use uparser_document_engine::DocumentFormat;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectionEvidence {
    ContainerOrSignature,
    DelimitedSyntax,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormatDetection {
    pub format: DocumentFormat,
    pub evidence: DetectionEvidence,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PreflightSource {
    bytes: Arc<[u8]>,
    filename_hint: Option<String>,
    digest: String,
    detection: FormatDetection,
}

#[derive(Debug, Default)]
struct CancellationState {
    cancelled: AtomicBool,
    notify: Notify,
}

#[derive(Debug, Clone, Default)]
pub struct CancellationToken(Arc<CancellationState>);

impl CancellationToken {
    pub fn cancel(&self) {
        if !self.0.cancelled.swap(true, Ordering::AcqRel) {
            self.0.notify.notify_waiters();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }

    pub async fn cancelled(&self) {
        let notified = self.0.notify.notified();
        if self.is_cancelled() {
            return;
        }
        notified.await;
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PageSourceError {
    #[error("page production was cancelled")]
    Cancelled,
    #[error("page production failed: {0}")]
    Production(String),
}

#[cfg(feature = "pdfium")]
pub struct PdfPageSource {
    bytes: Arc<[u8]>,
    digest: String,
    dpi: f32,
    pending_pages: VecDeque<u32>,
    total: usize,
    cancellation: CancellationToken,
}

#[cfg(feature = "pdfium")]
impl PdfPageSource {
    pub fn new(
        bytes: Arc<[u8]>,
        digest: impl Into<String>,
        dpi: f32,
        selected_pages: Option<&[u32]>,
        cancellation: CancellationToken,
    ) -> Result<Self, PageSourceError> {
        let page_count = crate::ingest::pdf_page_count(&bytes)
            .map_err(|error| PageSourceError::Production(error.to_string()))?;
        let pending_pages: VecDeque<u32> = match selected_pages {
            Some(selected) => selected
                .iter()
                .copied()
                .filter(|page| *page > 0 && *page <= page_count)
                .collect(),
            None => (1..=page_count).collect(),
        };
        let total = pending_pages.len();
        Ok(Self {
            bytes,
            digest: digest.into(),
            dpi,
            pending_pages,
            total,
            cancellation,
        })
    }
}

#[cfg(feature = "pdfium")]
#[async_trait::async_trait]
impl PageSource for PdfPageSource {
    fn format(&self) -> DocumentFormat {
        DocumentFormat::Pdf
    }

    fn content_digest(&self) -> &str {
        &self.digest
    }

    fn page_count_hint(&self) -> Option<usize> {
        Some(self.total)
    }

    async fn next_window(
        &mut self,
        max_pages: usize,
    ) -> Result<Vec<crate::ingest::RenderedPage>, PageSourceError> {
        if self.cancellation.is_cancelled() {
            return Err(PageSourceError::Cancelled);
        }
        let count = max_pages.max(1).min(self.pending_pages.len());
        let pages: Vec<u32> = self.pending_pages.drain(..count).collect();
        if pages.is_empty() {
            return Ok(Vec::new());
        }
        let rendered = crate::ingest::rasterize_pdf_page_numbers(&self.bytes, self.dpi, &pages)
            .map_err(|error| PageSourceError::Production(error.to_string()))?;
        if self.cancellation.is_cancelled() {
            return Err(PageSourceError::Cancelled);
        }
        Ok(rendered)
    }
}

pub fn pdf_page_source(
    bytes: Arc<[u8]>,
    digest: impl Into<String>,
    dpi: f32,
    selected_pages: Option<&[u32]>,
    cancellation: CancellationToken,
) -> Result<Box<dyn PageSource>, PageSourceError> {
    #[cfg(feature = "pdfium")]
    {
        Ok(Box::new(PdfPageSource::new(
            bytes,
            digest,
            dpi,
            selected_pages,
            cancellation,
        )?))
    }
    #[cfg(not(feature = "pdfium"))]
    {
        let _ = (bytes, digest.into(), dpi, selected_pages, cancellation);
        Err(PageSourceError::Production(
            "PDF rasterization requires the `pdfium` feature".into(),
        ))
    }
}

#[async_trait::async_trait]
pub trait PageSource: Send {
    fn format(&self) -> DocumentFormat;
    fn content_digest(&self) -> &str;
    fn page_count_hint(&self) -> Option<usize>;
    async fn next_window(
        &mut self,
        max_pages: usize,
    ) -> Result<Vec<crate::ingest::RenderedPage>, PageSourceError>;
}

pub struct MemoryPageSource {
    format: DocumentFormat,
    digest: String,
    total: usize,
    pages: VecDeque<crate::ingest::RenderedPage>,
    cancellation: CancellationToken,
}

impl MemoryPageSource {
    pub fn new(
        format: DocumentFormat,
        digest: impl Into<String>,
        pages: Vec<crate::ingest::RenderedPage>,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            format,
            digest: digest.into(),
            total: pages.len(),
            pages: pages.into(),
            cancellation,
        }
    }
}

#[async_trait::async_trait]
impl PageSource for MemoryPageSource {
    fn format(&self) -> DocumentFormat {
        self.format
    }

    fn content_digest(&self) -> &str {
        &self.digest
    }

    fn page_count_hint(&self) -> Option<usize> {
        Some(self.total)
    }

    async fn next_window(
        &mut self,
        max_pages: usize,
    ) -> Result<Vec<crate::ingest::RenderedPage>, PageSourceError> {
        if self.cancellation.is_cancelled() {
            return Err(PageSourceError::Cancelled);
        }
        let count = max_pages.max(1).min(self.pages.len());
        Ok(self.pages.drain(..count).collect())
    }
}

impl PreflightSource {
    pub fn new(bytes: impl Into<Arc<[u8]>>, filename_hint: Option<&str>) -> Self {
        let bytes = bytes.into();
        let format = uparser_document_engine::detect_format(&bytes, filename_hint);
        let digest = format!("{:x}", Sha256::digest(&bytes));
        let extension_format = uparser_document_engine::format_from_extension(filename_hint);
        let warnings = match (format, extension_format) {
            (detected, Some(hinted)) if detected.is_recognized() && detected != hinted => {
                vec![format!(
                    "content identifies {detected:?}, overriding filename extension for {hinted:?}"
                )]
            }
            (DocumentFormat::Unknown, Some(hinted)) => vec![format!(
                "filename suggests {hinted:?}, but the content could not be verified"
            )],
            _ => Vec::new(),
        };
        Self {
            bytes,
            filename_hint: filename_hint.map(str::to_owned),
            digest,
            detection: FormatDetection {
                format,
                evidence: match format {
                    DocumentFormat::Csv | DocumentFormat::Tsv => DetectionEvidence::DelimitedSyntax,
                    format if format.is_recognized() => DetectionEvidence::ContainerOrSignature,
                    _ => DetectionEvidence::Unknown,
                },
                warnings,
            },
        }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn shared_bytes(&self) -> Arc<[u8]> {
        Arc::clone(&self.bytes)
    }

    pub fn filename_hint(&self) -> Option<&str> {
        self.filename_hint.as_deref()
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }

    pub fn detection(&self) -> &FormatDetection {
        &self.detection
    }

    pub fn format(&self) -> DocumentFormat {
        self.detection.format
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authoritative_format_contract_has_sixteen_variants() {
        assert_eq!(DocumentFormat::ALL.len(), 16);
        assert_eq!(
            DocumentFormat::ALL
                .iter()
                .filter(|format| format.is_recognized())
                .count(),
            15
        );
    }

    #[test]
    fn source_detects_once_and_carries_stable_identity() {
        let first = PreflightSource::new(Arc::<[u8]>::from(&b"%PDF-1.7\n"[..]), Some("wrong.docx"));
        let second = PreflightSource::new(Arc::<[u8]>::from(&b"%PDF-1.7\n"[..]), Some("other.pdf"));
        assert_eq!(first.format(), DocumentFormat::Pdf);
        assert_eq!(first.digest(), second.digest());
        assert_eq!(first.bytes(), second.bytes());
        assert_eq!(first.detection().warnings.len(), 1);
    }

    #[test]
    fn unknown_content_is_not_promoted_by_extension() {
        let source = PreflightSource::new(&b"not a pdf"[..], Some("report.pdf"));
        assert_eq!(source.format(), DocumentFormat::Unknown);
        assert_eq!(source.detection().warnings.len(), 1);
    }

    #[tokio::test]
    async fn page_source_is_windowed_and_cancellable() {
        let cancellation = CancellationToken::default();
        let pages = (1..=3)
            .map(|page_num| crate::ingest::RenderedPage {
                page_num,
                width: 1,
                height: 1,
                png_bytes: Vec::new(),
            })
            .collect();
        let mut source =
            MemoryPageSource::new(DocumentFormat::Png, "digest", pages, cancellation.clone());
        assert_eq!(source.next_window(2).await.unwrap().len(), 2);
        cancellation.cancel();
        assert!(matches!(
            source.next_window(2).await,
            Err(PageSourceError::Cancelled)
        ));
    }

    #[cfg(feature = "pdfium")]
    #[tokio::test]
    async fn pdf_page_source_rasterizes_only_selected_window() {
        use std::fs;

        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../opensource/dots.ocr/demo/demo_pdf1.pdf"
        );
        let bytes: Arc<[u8]> = fs::read(fixture).unwrap().into();
        let cancellation = CancellationToken::default();
        let mut source =
            PdfPageSource::new(bytes, "fixture", 72.0, Some(&[2, 1]), cancellation).unwrap();

        let first = source.next_window(1).await.unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].page_num, 2);
        assert!(!first[0].png_bytes.is_empty());
        let second = source.next_window(1).await.unwrap();
        assert_eq!(second[0].page_num, 1);
        assert!(source.next_window(1).await.unwrap().is_empty());
    }
}
