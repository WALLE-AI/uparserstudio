//! Optional L3 document-level semantic classification for low-confidence
//! automatic routing. Failures preserve the deterministic L2 profile.

use crate::runner::{AnalysisArtifacts, AnalysisReport};
use crate::transport::{ChatCompletionRequest, Transport};
use crate::types::{AnalysisEvidence, DocumentGenre, DocumentKind, EvidenceSource, ProfileLevel};
use serde::Deserialize;
use std::time::Duration;

const CONFIDENCE_THRESHOLD: f32 = 0.65;
const MAX_SAMPLE_CHARS: usize = 12_000;

#[derive(Debug, Deserialize)]
struct SemanticPrediction {
    primary: DocumentGenre,
    #[serde(default)]
    tags: Vec<DocumentGenre>,
    confidence: f32,
}

pub fn should_escalate(report: &AnalysisReport) -> bool {
    report.profile.genre.confidence < CONFIDENCE_THRESHOLD && !sample_text(report).trim().is_empty()
}

pub async fn enrich_from_environment(report: &mut AnalysisReport) {
    let _ = enrich_from_environment_with_cancellation(
        report,
        crate::frontend::CancellationToken::default(),
    )
    .await;
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SemanticEnrichmentError {
    #[error("semantic classification cancelled")]
    Cancelled,
}

pub async fn enrich_from_environment_with_cancellation(
    report: &mut AnalysisReport,
    cancellation: crate::frontend::CancellationToken,
) -> Result<(), SemanticEnrichmentError> {
    if cancellation.is_cancelled() {
        return Err(SemanticEnrichmentError::Cancelled);
    }
    if !should_escalate(report) {
        return Ok(());
    }
    let Ok(endpoint) = std::env::var("UPARSER_CLASSIFIER_ENDPOINT") else {
        return Ok(());
    };
    let model = std::env::var("UPARSER_CLASSIFIER_MODEL").unwrap_or_else(|_| "model".into());
    let sample = sample_text(report);
    match classify(&endpoint, &model, &sample, cancellation).await {
        Ok(prediction) => apply_prediction(report, prediction),
        Err(ClassifyError::Cancelled) => return Err(SemanticEnrichmentError::Cancelled),
        Err(ClassifyError::Failed(error)) => report.profile.warnings.push(format!(
            "L3 semantic classification failed; retained L2 profile: {error}"
        )),
    }
    Ok(())
}

enum ClassifyError {
    Cancelled,
    Failed(String),
}

async fn classify(
    endpoint: &str,
    model: &str,
    sample: &str,
    cancellation: crate::frontend::CancellationToken,
) -> Result<SemanticPrediction, ClassifyError> {
    let request = ChatCompletionRequest {
        endpoint: endpoint.to_owned(),
        model: model.to_owned(),
        messages: vec![serde_json::json!({
            "role": "user",
            "content": format!(
                "Classify the document excerpt. Return JSON only with primary, tags, confidence. \
        Allowed genres: book, resume, tender, bid, legal_document, regulation, contract, academic_paper, \
        financial_report, manual, presentation, spreadsheet, general_report, other, unknown.\n\n{sample}"
            )
        })],
        sampling: serde_json::json!({"temperature": 0.0, "max_completion_tokens": 256}),
        timeout: Duration::from_secs(15),
        max_retries: 1,
    };
    let transport = Transport::new();
    let response = tokio::select! {
        biased;
        _ = cancellation.cancelled() => return Err(ClassifyError::Cancelled),
        result = transport.dispatch(request) => {
            result.map_err(|error| ClassifyError::Failed(error.to_string()))?
        }
    };
    let content =
        crate::adapters::extract_chat_content(&response).map_err(ClassifyError::Failed)?;
    decode_prediction(content).map_err(ClassifyError::Failed)
}

fn decode_prediction(content: &str) -> Result<SemanticPrediction, String> {
    let content = content
        .trim()
        .strip_prefix("```json")
        .or_else(|| content.trim().strip_prefix("```"))
        .unwrap_or(content.trim());
    let content = content.strip_suffix("```").unwrap_or(content).trim();
    let mut prediction: SemanticPrediction =
        serde_json::from_str(content).map_err(|error| error.to_string())?;
    prediction.confidence = prediction.confidence.clamp(0.0, 1.0);
    Ok(prediction)
}

fn sample_text(report: &AnalysisReport) -> String {
    let text = match &report.artifacts {
        AnalysisArtifacts::None => return String::new(),
        AnalysisArtifacts::Structured(document) => {
            uparser_document_engine::render::markdown(document)
        }
        #[cfg(feature = "native")]
        AnalysisArtifacts::Pdf(result) => result.markdown.clone().unwrap_or_default(),
    };
    text.chars().take(MAX_SAMPLE_CHARS).collect()
}

fn apply_prediction(report: &mut AnalysisReport, prediction: SemanticPrediction) {
    report.profile.genre.primary = prediction.primary;
    report.profile.genre.tags = prediction.tags;
    report.profile.genre.confidence = prediction.confidence;
    report.profile.genre.evidence.push(AnalysisEvidence {
        signal: "conditional L3 semantic classifier".into(),
        source: EvidenceSource::SemanticClassifier,
        unit_index: None,
        contribution: prediction.confidence,
    });
    report.profile.kind = legacy_kind(prediction.primary);
    report.profile.kind_confidence = prediction.confidence;
    report.profile.analysis_level = ProfileLevel::L3;
}

fn legacy_kind(genre: DocumentGenre) -> DocumentKind {
    match genre {
        DocumentGenre::Book => DocumentKind::Book,
        DocumentGenre::Resume => DocumentKind::Resume,
        DocumentGenre::Presentation => DocumentKind::Slide,
        DocumentGenre::Spreadsheet => DocumentKind::Spreadsheet,
        DocumentGenre::AcademicPaper => DocumentKind::AcademicPaper,
        DocumentGenre::GeneralReport | DocumentGenre::FinancialReport => DocumentKind::Report,
        _ => DocumentKind::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::DocumentFormat;
    use crate::types::{
        ContentMix, DocumentProfile, GenrePrediction, SourceQuality, StructureProfile,
    };
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

    fn report() -> AnalysisReport {
        AnalysisReport {
            profile: DocumentProfile {
                source_format: DocumentFormat::Docx,
                source_quality: SourceQuality::Structured,
                kind: DocumentKind::Unknown,
                kind_confidence: 0.2,
                genre: GenrePrediction {
                    primary: DocumentGenre::Unknown,
                    tags: vec![],
                    confidence: 0.2,
                    evidence: vec![],
                },
                structure: StructureProfile::default(),
                page_or_unit_count: Some(1),
                page_profiles: vec![],
                dominant_content: ContentMix::TextDominant,
                analysis_level: ProfileLevel::L2,
                warnings: vec![],
            },
            artifacts: AnalysisArtifacts::None,
        }
    }

    #[test]
    fn prediction_promotes_profile_to_l3() {
        let mut report = report();
        apply_prediction(
            &mut report,
            SemanticPrediction {
                primary: DocumentGenre::Tender,
                tags: vec![DocumentGenre::Contract],
                confidence: 0.9,
            },
        );
        assert_eq!(report.profile.analysis_level, ProfileLevel::L3);
        assert_eq!(report.profile.genre.primary, DocumentGenre::Tender);
        assert_eq!(report.profile.genre.tags, vec![DocumentGenre::Contract]);
        assert_eq!(
            report.profile.genre.evidence[0].source,
            EvidenceSource::SemanticClassifier
        );
    }

    #[test]
    fn no_text_never_escalates() {
        assert!(!should_escalate(&report()));
    }

    #[test]
    fn fenced_classifier_json_is_accepted_and_confidence_is_clamped() {
        let prediction = decode_prediction(
            "```json\n{\"primary\":\"resume\",\"tags\":[],\"confidence\":1.2}\n```",
        )
        .unwrap();
        assert_eq!(prediction.primary, DocumentGenre::Resume);
        assert_eq!(prediction.confidence, 1.0);

        let plain = decode_prediction(
            "{\"primary\":\"contract\",\"tags\":[\"tender\"],\"confidence\":-0.2}",
        )
        .unwrap();
        assert_eq!(plain.primary, DocumentGenre::Contract);
        assert_eq!(plain.tags, [DocumentGenre::Tender]);
        assert_eq!(plain.confidence, 0.0);
        assert!(decode_prediction("```json\nnot json\n```").is_err());
    }

    #[test]
    fn structured_samples_escalate_and_are_bounded() {
        let mut value = report();
        let mut document = uparser_document_engine::CanonicalDocument::new(DocumentFormat::Docx);
        let mut unit = uparser_document_engine::DocumentUnit::new(
            uparser_document_engine::UnitKind::Flow,
            0,
            None,
        );
        unit.blocks.push(uparser_document_engine::Block::paragraph(
            "x".repeat(MAX_SAMPLE_CHARS + 50),
        ));
        document.units.push(unit);
        value.artifacts = AnalysisArtifacts::Structured(document);

        assert!(should_escalate(&value));
        assert_eq!(sample_text(&value).chars().count(), MAX_SAMPLE_CHARS);
    }

    #[test]
    fn legacy_kind_mapping_is_exhaustive_for_compatibility_categories() {
        assert_eq!(legacy_kind(DocumentGenre::Book), DocumentKind::Book);
        assert_eq!(legacy_kind(DocumentGenre::Resume), DocumentKind::Resume);
        assert_eq!(
            legacy_kind(DocumentGenre::Presentation),
            DocumentKind::Slide
        );
        assert_eq!(
            legacy_kind(DocumentGenre::Spreadsheet),
            DocumentKind::Spreadsheet
        );
        assert_eq!(
            legacy_kind(DocumentGenre::AcademicPaper),
            DocumentKind::AcademicPaper
        );
        assert_eq!(
            legacy_kind(DocumentGenre::FinancialReport),
            DocumentKind::Report
        );
        assert_eq!(legacy_kind(DocumentGenre::Contract), DocumentKind::Unknown);
    }

    #[tokio::test]
    async fn classifier_decodes_success_and_reports_invalid_payloads() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": {
                        "content": "{\"primary\":\"manual\",\"tags\":[],\"confidence\":0.8}"
                    }
                }]
            })))
            .mount(&server)
            .await;
        let prediction = match classify(
            &server.uri(),
            "classifier",
            "document sample",
            crate::frontend::CancellationToken::default(),
        )
        .await
        {
            Ok(prediction) => prediction,
            Err(_) => panic!("valid classifier response"),
        };
        assert_eq!(prediction.primary, DocumentGenre::Manual);

        let invalid = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{"finish_reason": "stop", "message": {"content": "not json"}}]
            })))
            .mount(&invalid)
            .await;
        assert!(matches!(
            classify(
                &invalid.uri(),
                "classifier",
                "document sample",
                crate::frontend::CancellationToken::default(),
            )
            .await,
            Err(ClassifyError::Failed(_))
        ));
    }

    #[tokio::test]
    async fn cancellation_interrupts_an_in_flight_l3_request() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(2)))
            .mount(&server)
            .await;
        let cancellation = crate::frontend::CancellationToken::default();
        let trigger = cancellation.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            trigger.cancel();
        });

        assert!(matches!(
            classify(&server.uri(), "classifier", "document sample", cancellation).await,
            Err(ClassifyError::Cancelled)
        ));
    }
}
