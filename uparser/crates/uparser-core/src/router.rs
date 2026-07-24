//! Document-level protocol routing, per ARCHITECTURE.md §13.4 / T-8.2.
//! Matches a `DocumentProfile` (from `profiler.rs`) against the v1
//! routing table — document-level only, always terminates (a fallback
//! row guarantees a decision, never a panic). XLSX/CSV structured
//! sources never reach this function in the real control flow (they're
//! bypassed by `ingest::structured_bypass` before profiler/router run,
//! per §13.1a) — `route()` still handles an xlsx-sourced profile
//! gracefully if called directly, it just isn't the real dispatch path
//! for that case.

use crate::types::{ContentMix, DocumentKind, DocumentProfile};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteDecision {
    pub protocol: &'static str,
    pub reason: String,
}

/// Match `profile` against the routing table (§13.4), in order. The last
/// row is an unconditional fallback, so this always returns a decision.
pub fn route(profile: &DocumentProfile) -> RouteDecision {
    if profile.dominant_content == ContentMix::TextDominant {
        return RouteDecision {
            protocol: "native",
            reason:
                "digitally-native text-dominant document: zero-model native extraction suffices"
                    .to_string(),
        };
    }

    if profile.kind == DocumentKind::Slide {
        return RouteDecision {
            protocol: "mineru-vlm",
            reason: "slide/presentation source: VLM semantic understanding of title+bullet+image layout preferred over geometric projection"
                .to_string(),
        };
    }

    if profile.kind == DocumentKind::Resume && profile.dominant_content == ContentMix::Mixed {
        return RouteDecision {
            protocol: "mineru-vlm",
            reason:
                "fragmented multi-column layout (resume-like): VLM semantic understanding preferred"
                    .to_string(),
        };
    }

    if profile.dominant_content == ContentMix::TableDense {
        return RouteDecision {
            protocol: "pipeline",
            reason: "table-dense document: dedicated table-stage recognition preferred (routing intent recorded — final \
                      protocol choice deferred to real-sample evaluation per §13.4; `pipeline` adapter doesn't exist yet)"
                .to_string(),
        };
    }

    let has_chart_region = profile.page_profiles.iter().any(|p| p.has_chart_region);
    if has_chart_region {
        return RouteDecision {
            protocol: "mineru-vlm",
            reason: "chart/figure regions present: VLM's descriptive-caption capability is the only low-cost option \
                      (a description, not precise data extraction)"
                .to_string(),
        };
    }

    RouteDecision {
        protocol: "mineru-vlm",
        reason: "unable to reliably classify document; using default protocol".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::DocumentFormat;
    use crate::types::{PageProfile, ProfileLevel};

    fn base_profile(kind: DocumentKind, dominant_content: ContentMix) -> DocumentProfile {
        DocumentProfile {
            source_format: DocumentFormat::Pdf,
            kind,
            kind_confidence: 0.8,
            page_profiles: vec![],
            dominant_content,
        }
    }

    fn page(has_table_region: bool, has_chart_region: bool) -> PageProfile {
        PageProfile {
            text_density: 0.3,
            image_density: 0.1,
            has_table_region,
            table_subtype: None,
            has_chart_region,
            chart_subtype: None,
            profile_level: ProfileLevel::L2,
        }
    }

    #[test]
    fn text_dominant_routes_to_native() {
        let profile = base_profile(DocumentKind::Report, ContentMix::TextDominant);
        assert_eq!(route(&profile).protocol, "native");
    }

    #[test]
    fn slide_routes_to_mineru_vlm() {
        let profile = base_profile(DocumentKind::Slide, ContentMix::Mixed);
        assert_eq!(route(&profile).protocol, "mineru-vlm");
    }

    #[test]
    fn resume_mixed_routes_to_mineru_vlm() {
        let profile = base_profile(DocumentKind::Resume, ContentMix::Mixed);
        assert_eq!(route(&profile).protocol, "mineru-vlm");
    }

    #[test]
    fn table_dense_routes_to_pipeline() {
        let profile = base_profile(DocumentKind::Unknown, ContentMix::TableDense);
        assert_eq!(route(&profile).protocol, "pipeline");
    }

    #[test]
    fn chart_region_routes_to_mineru_vlm() {
        let mut profile = base_profile(DocumentKind::Unknown, ContentMix::ImageDense);
        profile.page_profiles = vec![page(false, true)];
        assert_eq!(route(&profile).protocol, "mineru-vlm");
    }

    #[test]
    fn unclassifiable_falls_back_to_mineru_vlm_with_warning_reason() {
        let profile = base_profile(DocumentKind::Unknown, ContentMix::ImageDense);
        let decision = route(&profile);
        assert_eq!(decision.protocol, "mineru-vlm");
        assert!(decision.reason.contains("unable to reliably classify"));
    }

    #[test]
    fn xlsx_sourced_profile_does_not_panic() {
        // Real xlsx short-circuit lives in ingest::structured_bypass, not
        // here — router must still degrade gracefully if called on one.
        let mut profile = base_profile(DocumentKind::Spreadsheet, ContentMix::TableDense);
        profile.source_format = DocumentFormat::Xlsx;
        let decision = route(&profile);
        assert_eq!(decision.protocol, "pipeline");
    }
}
