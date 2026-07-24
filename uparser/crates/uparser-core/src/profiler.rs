//! Document pre-analysis, per ARCHITECTURE.md §13.1a-§13.3 / T-8.1. Two
//! layers, both free of model calls: L1 (format/metadata, always
//! available) and L2 (structural heuristics, requires the actual PDF
//! bytes — feature-gated on `native` since it reuses
//! `liteparse::LiteParse::is_complex()` directly rather than
//! reimplementing pixel-based heuristics from scratch, per the
//! architecture doc's explicit instruction to "复用/移植 liteparse 已有的
//! is_complex()"). L3 (deep semantic classification via a model call) is
//! opt-in per §13.2a and not implemented here.

use crate::ingest::DocumentFormat;
use crate::types::{ContentMix, DocumentKind, DocumentProfile};
#[cfg(feature = "native")]
use crate::types::{PageProfile, ProfileLevel};

/// L1: near-zero-cost, unreliable classification from format alone. A
/// `Pdf`/`Docx` alone tells you almost nothing — only formats with a
/// strong format→kind prior (presentations, spreadsheets) get anything
/// above minimal confidence.
pub fn profile_l1(format: DocumentFormat) -> DocumentProfile {
    let (kind, kind_confidence, dominant_content) = match format {
        DocumentFormat::Pptx => (DocumentKind::Slide, 0.6, ContentMix::Mixed),
        DocumentFormat::Xlsx | DocumentFormat::Csv => {
            (DocumentKind::Spreadsheet, 0.9, ContentMix::TableDense)
        }
        _ => (DocumentKind::Unknown, 0.1, ContentMix::Mixed),
    };

    DocumentProfile {
        source_format: format,
        kind,
        kind_confidence,
        page_profiles: vec![],
        dominant_content,
    }
}

#[cfg(feature = "native")]
mod l2 {
    use super::*;
    use crate::ingest::IngestError;
    use crate::types::{ChartSubtype, TableSubtype};
    use liteparse::ocr_merge::PageComplexityStats;
    use liteparse::{LiteParse, LiteParseConfig};

    /// Figure-region coverage above which a dense-graphics region is
    /// treated as a chart-region proxy — mirrors liteparse's own
    /// `DENSE_GRAPHICS_MIN_COVERAGE` threshold (0.2), the same constant
    /// that drives its `DenseGraphics` layout-complexity reason.
    const CHART_PROXY_MIN_FIGURE_COVERAGE: f32 = 0.2;

    /// L2: structural heuristics from the real PDF text layer + layout
    /// pass, via liteparse's `is_complex()`. Pure computation once
    /// liteparse's output is in hand — no new model/network calls.
    pub async fn profile_l2(
        pdf_bytes: &[u8],
        format: DocumentFormat,
    ) -> Result<DocumentProfile, IngestError> {
        let config = LiteParseConfig {
            ocr_enabled: false,
            ..Default::default()
        };
        let parser = LiteParse::new(config);
        let input = liteparse::types::PdfInput::Bytes(pdf_bytes.to_vec());

        let stats = parser
            .is_complex(input)
            .await
            .map_err(|e| IngestError::Profiling(e.to_string()))?;

        let page_profiles: Vec<PageProfile> = stats.iter().map(map_page_stats).collect();
        let (kind, kind_confidence, dominant_content) = aggregate(&stats, &page_profiles);

        Ok(DocumentProfile {
            source_format: format,
            kind,
            kind_confidence,
            page_profiles,
            dominant_content,
        })
    }

    fn map_page_stats(stats: &PageComplexityStats) -> PageProfile {
        let layout = stats.layout.as_ref();
        let has_table_region = layout
            .map(|l| l.ruled_table_count > 0 || l.text_table_run_count > 0)
            .unwrap_or(false);
        let figure_coverage = layout.map(|l| l.figure_coverage).unwrap_or(0.0);
        // Not a confident chart classifier — just the closest free proxy
        // L2 can honestly claim. True chart detection is L3's job.
        let has_chart_region =
            figure_coverage >= CHART_PROXY_MIN_FIGURE_COVERAGE && !has_table_region;

        PageProfile {
            text_density: stats.text_coverage,
            image_density: stats.image_coverage,
            has_table_region,
            table_subtype: None::<TableSubtype>,
            has_chart_region,
            chart_subtype: None::<ChartSubtype>,
            profile_level: ProfileLevel::L2,
        }
    }

    fn aggregate(
        stats: &[PageComplexityStats],
        pages: &[PageProfile],
    ) -> (DocumentKind, f32, ContentMix) {
        if stats.is_empty() {
            return (DocumentKind::Unknown, 0.1, ContentMix::Mixed);
        }
        let n = stats.len() as f32;

        let low_ocr_frac = stats.iter().filter(|s| !s.needs_ocr).count() as f32 / n;
        let high_text_frac = pages.iter().filter(|p| p.text_density >= 0.15).count() as f32 / n;
        let table_frac = pages.iter().filter(|p| p.has_table_region).count() as f32 / n;
        let chart_frac = pages.iter().filter(|p| p.has_chart_region).count() as f32 / n;
        let avg_image_density = pages.iter().map(|p| p.image_density).sum::<f32>() / n;
        let multi_column = stats
            .iter()
            .any(|s| s.layout.as_ref().is_some_and(|l| l.column_count >= 2));

        let dominant_content = if table_frac > 0.5 {
            ContentMix::TableDense
        } else if chart_frac > 0.5 || avg_image_density > 0.3 {
            ContentMix::ImageDense
        } else if low_ocr_frac > 0.7 && high_text_frac > 0.7 {
            ContentMix::TextDominant
        } else {
            ContentMix::Mixed
        };

        // Book vs Report vs AcademicPaper disambiguation is genuinely
        // L3 territory (semantic, not structural) — L2 only claims the
        // generic long-text-dominant bucket (Report) rather than
        // overclaiming a finer kind it can't actually tell apart.
        let (kind, kind_confidence) = if dominant_content == ContentMix::TextDominant {
            (DocumentKind::Report, 0.8)
        } else if stats.len() <= 2 && multi_column {
            (DocumentKind::Resume, 0.6)
        } else {
            (DocumentKind::Unknown, 0.3)
        };

        (kind, kind_confidence, dominant_content)
    }
}

#[cfg(feature = "native")]
pub use l2::profile_l2;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l1_pptx_is_slide() {
        let profile = profile_l1(DocumentFormat::Pptx);
        assert_eq!(profile.kind, DocumentKind::Slide);
        assert!(profile.kind_confidence > 0.0);
        assert!(profile.page_profiles.is_empty());
    }

    #[test]
    fn l1_xlsx_is_spreadsheet_table_dense() {
        let profile = profile_l1(DocumentFormat::Xlsx);
        assert_eq!(profile.kind, DocumentKind::Spreadsheet);
        assert_eq!(profile.dominant_content, ContentMix::TableDense);
    }

    #[test]
    fn l1_csv_is_spreadsheet() {
        let profile = profile_l1(DocumentFormat::Csv);
        assert_eq!(profile.kind, DocumentKind::Spreadsheet);
    }

    #[test]
    fn l1_pdf_is_unknown_with_low_confidence() {
        let profile = profile_l1(DocumentFormat::Pdf);
        assert_eq!(profile.kind, DocumentKind::Unknown);
        assert!(profile.kind_confidence < 0.5);
    }

    #[test]
    fn l1_docx_is_unknown_with_low_confidence() {
        let profile = profile_l1(DocumentFormat::Docx);
        assert_eq!(profile.kind, DocumentKind::Unknown);
        assert!(profile.kind_confidence < 0.5);
    }

    #[cfg(feature = "native")]
    mod l2_tests {
        use super::*;

        fn fixture_pdf_path() -> String {
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../../opensource/liteparse/integration_tests_data/sample.pdf"
            )
            .to_string()
        }

        #[tokio::test]
        async fn l2_text_dominant_pdf_profiles_as_text_dominant() {
            let path = fixture_pdf_path();
            if !std::path::Path::new(&path).exists() {
                eprintln!("skipping: no fixture PDF at {path}");
                return;
            }
            let bytes = std::fs::read(&path).expect("read fixture PDF");

            let profile = profile_l2(&bytes, DocumentFormat::Pdf)
                .await
                .expect("profile_l2 succeeds");

            assert!(!profile.page_profiles.is_empty());
            assert!(
                profile
                    .page_profiles
                    .iter()
                    .all(|p| p.profile_level == ProfileLevel::L2)
            );
            // The fixture is a genuine digitally-native text document —
            // the actual "digitally-native long text -> native" claim
            // this phase exists to validate (T-8.1's acceptance
            // criterion).
            assert_eq!(profile.dominant_content, ContentMix::TextDominant);
            assert!(profile.kind_confidence > 0.5);
        }

        #[tokio::test]
        async fn l2_empty_document_is_unknown_not_a_panic() {
            // Malformed/empty PDF bytes: is_complex should error cleanly,
            // not panic.
            let result = profile_l2(b"not a pdf", DocumentFormat::Pdf).await;
            assert!(result.is_err());
        }
    }
}
