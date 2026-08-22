//! Explainable document-level routing.

use crate::types::{ContentMix, DocumentGenre, DocumentProfile, SourceQuality};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteReasonCode {
    SourceSemantic,
    NativeText,
    VisualRequired,
    PresentationLayout,
    ResumeLayout,
    TableSpecialist,
    GenreStructure,
    Unavailable,
    ConservativeFallback,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteCandidate {
    pub protocol: String,
    pub score: i32,
    pub feasible: bool,
    pub reason_codes: Vec<RouteReasonCode>,
    pub rejection: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteDecision {
    pub protocol: String,
    #[serde(default)]
    pub origin: RouteOrigin,
    pub reason: String,
    pub confidence: f32,
    pub candidates: Vec<RouteCandidate>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteOrigin {
    #[default]
    Auto,
    Explicit,
}

#[derive(Debug, Clone, Copy)]
pub struct RoutingEnvironment {
    pub native: bool,
    pub local_ocr: bool,
    pub model_protocol: bool,
    pub pipeline: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum RoutePreference {
    #[default]
    Quality,
    Speed,
    Cost,
}

impl Default for RoutingEnvironment {
    fn default() -> Self {
        Self {
            native: cfg!(feature = "native"),
            local_ocr: cfg!(feature = "pdfium") && crate::adapters::local_tesseract::available(),
            model_protocol: true,
            // Mode 3 exists as an explicit compatibility adapter, but is not
            // auto-feasible until its stage deployment and G-R gate are known.
            pipeline: false,
        }
    }
}

pub fn route(profile: &DocumentProfile) -> RouteDecision {
    route_with_preference(
        profile,
        RoutingEnvironment::default(),
        RoutePreference::Quality,
    )
}

pub fn route_with_environment(
    profile: &DocumentProfile,
    environment: RoutingEnvironment,
) -> RouteDecision {
    route_with_preference(profile, environment, RoutePreference::Quality)
}

pub fn route_with_preference(
    profile: &DocumentProfile,
    environment: RoutingEnvironment,
    preference: RoutePreference,
) -> RouteDecision {
    let mut candidates = vec![
        native_candidate(profile, environment.native),
        local_ocr_candidate(profile, environment.local_ocr),
        model_candidate(profile, environment.model_protocol),
        pipeline_candidate(profile, environment.pipeline),
    ];
    for candidate in &mut candidates {
        candidate.score += preference_adjustment(preference, &candidate.protocol);
    }
    candidates.sort_by(|left, right| {
        right
            .feasible
            .cmp(&left.feasible)
            .then_with(|| right.score.cmp(&left.score))
            .then_with(|| left.protocol.cmp(&right.protocol))
    });
    let selected = candidates
        .iter()
        .find(|candidate| candidate.feasible)
        .expect("model routing environment must expose at least one mode");
    let runner_up = candidates
        .iter()
        .filter(|candidate| candidate.feasible)
        .nth(1)
        .map(|candidate| candidate.score)
        .unwrap_or(selected.score - 50);
    let confidence = ((selected.score - runner_up).max(0) as f32 / 50.0).clamp(0.2, 1.0);
    let reason = format!(
        "selected {} (score {}): {}",
        selected.protocol,
        selected.score,
        selected
            .reason_codes
            .iter()
            .map(|reason| format!("{reason:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    RouteDecision {
        protocol: selected.protocol.clone(),
        origin: RouteOrigin::Auto,
        reason,
        confidence,
        candidates,
    }
}

fn preference_adjustment(preference: RoutePreference, protocol: &str) -> i32 {
    match (preference, protocol) {
        (RoutePreference::Quality, "pipeline") => 10,
        (RoutePreference::Quality, "mineru-vlm") => 5,
        (RoutePreference::Speed, "native") => 35,
        (RoutePreference::Speed, "mineru-vlm") => -10,
        (RoutePreference::Speed, "pipeline") => -20,
        (RoutePreference::Cost, "native") => 45,
        (RoutePreference::Speed | RoutePreference::Cost, "tesseract") => 20,
        (RoutePreference::Cost, "mineru-vlm") => -20,
        (RoutePreference::Cost, "pipeline") => -10,
        _ => 0,
    }
}

fn local_ocr_candidate(profile: &DocumentProfile, available: bool) -> RouteCandidate {
    let applicable = matches!(
        profile.source_quality,
        SourceQuality::Scanned | SourceQuality::ImageOnly
    );
    RouteCandidate {
        protocol: "tesseract".to_owned(),
        score: if applicable { 130 } else { -100 },
        feasible: available && applicable,
        reason_codes: if applicable {
            vec![RouteReasonCode::VisualRequired]
        } else {
            Vec::new()
        },
        rejection: (!(available && applicable)).then(|| {
            if !available {
                "local Tesseract or PDF rasterization is unavailable".to_owned()
            } else {
                "local OCR is reserved for scanned/image-only sources".to_owned()
            }
        }),
    }
}

fn native_candidate(profile: &DocumentProfile, available: bool) -> RouteCandidate {
    let source_supported = !matches!(
        profile.source_quality,
        SourceQuality::Scanned | SourceQuality::ImageOnly | SourceQuality::Unknown
    );
    let source_semantic = profile.source_quality == SourceQuality::Structured;
    let feasible = (available || source_semantic) && source_supported;
    let mut score = 35;
    let mut reasons = Vec::new();
    match profile.source_quality {
        SourceQuality::Structured => {
            score += 45;
            reasons.push(RouteReasonCode::SourceSemantic);
        }
        SourceQuality::NativeText => {
            score += 45;
            reasons.push(RouteReasonCode::NativeText);
        }
        _ => score -= 100,
    }
    if profile.dominant_content == ContentMix::TextDominant {
        score += 20;
    }
    if profile.genre.primary == DocumentGenre::Spreadsheet {
        score += 35;
    }
    if matches!(
        profile.genre.primary,
        DocumentGenre::Book
            | DocumentGenre::Regulation
            | DocumentGenre::LegalDocument
            | DocumentGenre::Contract
    ) {
        score += 15;
        reasons.push(RouteReasonCode::GenreStructure);
    }
    if profile.genre.primary == DocumentGenre::Presentation {
        score -= 35;
    }
    if profile.genre.primary == DocumentGenre::Resume && profile.structure.multi_column_ratio > 0.0
    {
        score -= 30;
    }
    RouteCandidate {
        protocol: "native".to_owned(),
        score,
        feasible,
        reason_codes: reasons,
        rejection: (!feasible).then(|| {
            if !available && !source_semantic {
                "native feature is not compiled".to_owned()
            } else {
                "source has no reliable native text or source semantics".to_owned()
            }
        }),
    }
}

fn model_candidate(profile: &DocumentProfile, available: bool) -> RouteCandidate {
    let mut score = 50;
    let mut reasons = Vec::new();
    if matches!(
        profile.source_quality,
        SourceQuality::Scanned | SourceQuality::ImageOnly | SourceQuality::Mixed
    ) {
        score += 45;
        reasons.push(RouteReasonCode::VisualRequired);
    }
    match profile.genre.primary {
        DocumentGenre::Presentation => {
            score += 35;
            reasons.push(RouteReasonCode::PresentationLayout);
        }
        DocumentGenre::Resume => {
            score += 25;
            reasons.push(RouteReasonCode::ResumeLayout);
        }
        DocumentGenre::Unknown => {
            score += 10;
            reasons.push(RouteReasonCode::ConservativeFallback);
        }
        _ => {}
    }
    RouteCandidate {
        protocol: "mineru-vlm".to_owned(),
        score,
        feasible: available,
        reason_codes: reasons,
        rejection: (!available).then(|| "model protocol endpoint is unavailable".to_owned()),
    }
}

fn pipeline_candidate(profile: &DocumentProfile, available: bool) -> RouteCandidate {
    let mut score = 30;
    let mut reasons = Vec::new();
    if profile.dominant_content == ContentMix::TableDense {
        score += 60;
        reasons.push(RouteReasonCode::TableSpecialist);
    }
    if matches!(
        profile.genre.primary,
        DocumentGenre::Tender | DocumentGenre::Bid | DocumentGenre::FinancialReport
    ) {
        score += 25;
        reasons.push(RouteReasonCode::GenreStructure);
    }
    RouteCandidate {
        protocol: "pipeline".to_owned(),
        score,
        feasible: available,
        reason_codes: reasons,
        rejection: (!available).then(|| "pipeline stages are unavailable".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::DocumentFormat;
    use crate::types::{DocumentKind, GenrePrediction, ProfileLevel, StructureProfile};

    fn profile(genre: DocumentGenre, quality: SourceQuality, mix: ContentMix) -> DocumentProfile {
        DocumentProfile {
            source_format: DocumentFormat::Pdf,
            source_quality: quality,
            kind: DocumentKind::Unknown,
            kind_confidence: 0.8,
            genre: GenrePrediction {
                primary: genre,
                tags: Vec::new(),
                confidence: 0.8,
                evidence: Vec::new(),
            },
            structure: StructureProfile::default(),
            page_or_unit_count: None,
            page_profiles: Vec::new(),
            dominant_content: mix,
            analysis_level: ProfileLevel::L2,
            warnings: Vec::new(),
        }
    }

    fn all_available() -> RoutingEnvironment {
        RoutingEnvironment {
            native: true,
            local_ocr: true,
            model_protocol: true,
            pipeline: true,
        }
    }

    #[test]
    fn structured_spreadsheet_routes_native() {
        let p = profile(
            DocumentGenre::Spreadsheet,
            SourceQuality::Structured,
            ContentMix::TableDense,
        );
        assert_eq!(
            route_with_environment(&p, all_available()).protocol,
            "native"
        );
    }

    #[test]
    fn scanned_document_routes_local_ocr() {
        let p = profile(
            DocumentGenre::Unknown,
            SourceQuality::Scanned,
            ContentMix::ImageDense,
        );
        let decision = route_with_environment(&p, all_available());
        assert_eq!(decision.protocol, "tesseract");
        assert!(
            !decision
                .candidates
                .iter()
                .find(|c| c.protocol == "native")
                .unwrap()
                .feasible
        );
    }

    #[test]
    fn scanned_document_falls_back_to_model_without_local_ocr() {
        let p = profile(
            DocumentGenre::Unknown,
            SourceQuality::Scanned,
            ContentMix::ImageDense,
        );
        let decision = route_with_environment(
            &p,
            RoutingEnvironment {
                local_ocr: false,
                ..all_available()
            },
        );
        assert_eq!(decision.protocol, "mineru-vlm");
    }

    #[test]
    fn table_dense_tender_routes_pipeline() {
        let p = profile(
            DocumentGenre::Tender,
            SourceQuality::Structured,
            ContentMix::TableDense,
        );
        assert_eq!(
            route_with_environment(&p, all_available()).protocol,
            "pipeline"
        );
    }

    #[test]
    fn presentation_routes_model_even_when_structured() {
        let p = profile(
            DocumentGenre::Presentation,
            SourceQuality::Structured,
            ContentMix::Mixed,
        );
        assert_eq!(
            route_with_environment(&p, all_available()).protocol,
            "mineru-vlm"
        );
    }

    #[test]
    fn unavailable_high_score_candidate_is_rejected() {
        let p = profile(
            DocumentGenre::Tender,
            SourceQuality::Structured,
            ContentMix::TableDense,
        );
        let env = RoutingEnvironment {
            pipeline: false,
            ..all_available()
        };
        let decision = route_with_environment(&p, env);
        assert_ne!(decision.protocol, "pipeline");
        assert!(decision.candidates.iter().any(|candidate| {
            candidate.protocol == "pipeline" && candidate.rejection.is_some()
        }));
    }

    #[test]
    fn preference_changes_scores_but_not_feasibility() {
        let p = profile(
            DocumentGenre::Presentation,
            SourceQuality::Structured,
            ContentMix::Mixed,
        );
        let speed = route_with_preference(&p, all_available(), RoutePreference::Speed);
        let quality = route_with_preference(&p, all_available(), RoutePreference::Quality);
        let speed_native = speed
            .candidates
            .iter()
            .find(|candidate| candidate.protocol == "native")
            .unwrap();
        let quality_native = quality
            .candidates
            .iter()
            .find(|candidate| candidate.protocol == "native")
            .unwrap();
        assert!(speed_native.score > quality_native.score);

        let unavailable = route_with_preference(
            &p,
            RoutingEnvironment {
                pipeline: false,
                ..all_available()
            },
            RoutePreference::Quality,
        );
        assert!(
            !unavailable
                .candidates
                .iter()
                .find(|candidate| candidate.protocol == "pipeline")
                .unwrap()
                .feasible
        );
    }
}
