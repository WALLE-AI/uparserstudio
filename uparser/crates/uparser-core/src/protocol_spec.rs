//! Declarative catalog for protocol execution shapes and wire contracts.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModeKind {
    Native,
    ModelProtocol,
    Pipeline,
    Test,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolShape {
    NativeDocument,
    OneShotPage,
    LayoutThenRecognize,
    StructuredService,
    StageGraph,
    Mock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportContract {
    None,
    OpenAiChatCompletions,
    PaddleOcrService,
    PipelineStages,
    InProcess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreprocessKind {
    SourceSemantic,
    HardResize,
    SmartResize,
    PixelBounds,
    PageImage,
    StageGraph,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecodeKind {
    NativeArtifact,
    CustomToken,
    StrictJson,
    PythonLiteral,
    OcrBoxes,
    Markdown,
    StructuredEnvelope,
    StageOutputs,
    /// NaviDC-OCR's line-oriented `<box:...><label:...><direction>`
    /// layout grammar — distinct from `CustomToken` (mineru-vlm) and
    /// `PythonLiteral` (MonkeyOCRv2), which share neither syntax nor
    /// parser with it.
    NaviLayoutTokens,
    Mock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoordinateKind {
    SourceSemantic,
    Norm0To1000,
    PixelAbs,
    FullPage,
    /// The protocol reports no per-block geometry at all — it answers with a
    /// finished Markdown document, which has none. Distinct from `FullPage`:
    /// that one *does* give a box (the page), this one gives nothing, and a
    /// consumer needs to know which before it reads `bbox_px`.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderSource {
    SourceSemantic,
    FromModel,
    AdapterComputed,
    GeometricFallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProtocolSpec {
    pub name: &'static str,
    pub mode: ModeKind,
    pub shape: ProtocolShape,
    pub transport: TransportContract,
    pub preprocess: PreprocessKind,
    pub decode: DecodeKind,
    pub coordinates: CoordinateKind,
    pub order: OrderSource,
    /// The single source of truth for this protocol's built-in fallback
    /// endpoint. Each adapter's `Default` impl reads it from here rather
    /// than carrying its own literal, so there is exactly one place to
    /// change and no pair of copies that can silently drift apart.
    pub default_endpoint: Option<&'static str>,
    /// Built-in fallback model name. `None` for protocols whose service
    /// contract has no model field at all (`paddleocr`,
    /// `paddlex-structure`, `pipeline`) as well as for the non-HTTP ones.
    pub default_model: Option<&'static str>,
    /// Per-request timeout and retry budget. These differ per protocol for
    /// historical reasons (see each entry's comment); they are recorded
    /// here as-is rather than unified, since changing them would alter real
    /// behaviour. Both are overridable via config/`AdapterOverrides`.
    pub default_timeout_secs: u64,
    pub default_max_retries: u32,
    pub requires_pdf_native_feature: bool,
}

pub const PROTOCOL_SPECS: &[ProtocolSpec] = &[
    ProtocolSpec {
        name: "native",
        mode: ModeKind::Native,
        shape: ProtocolShape::NativeDocument,
        transport: TransportContract::InProcess,
        preprocess: PreprocessKind::SourceSemantic,
        decode: DecodeKind::NativeArtifact,
        coordinates: CoordinateKind::SourceSemantic,
        order: OrderSource::SourceSemantic,
        // In-process: no endpoint, no model, no network timeout/retry.
        default_endpoint: None,
        default_model: None,
        default_timeout_secs: 0,
        default_max_retries: 0,
        requires_pdf_native_feature: true,
    },
    ProtocolSpec {
        name: "tesseract",
        mode: ModeKind::Native,
        shape: ProtocolShape::OneShotPage,
        transport: TransportContract::InProcess,
        preprocess: PreprocessKind::PageImage,
        decode: DecodeKind::OcrBoxes,
        coordinates: CoordinateKind::PixelAbs,
        order: OrderSource::FromModel,
        // Local subprocess: language comes from `UPARSER_OCR_LANG`, not
        // from the model field.
        default_endpoint: None,
        default_model: None,
        default_timeout_secs: 0,
        default_max_retries: 0,
        requires_pdf_native_feature: false,
    },
    ProtocolSpec {
        name: "mineru-vlm",
        mode: ModeKind::ModelProtocol,
        shape: ProtocolShape::LayoutThenRecognize,
        transport: TransportContract::OpenAiChatCompletions,
        preprocess: PreprocessKind::HardResize,
        decode: DecodeKind::CustomToken,
        coordinates: CoordinateKind::Norm0To1000,
        order: OrderSource::GeometricFallback,
        default_endpoint: Some("http://localhost:8000/v1/chat/completions"),
        default_model: Some("mineru-vlm"),
        // 60s rather than the 120s most protocols use. Preserved as-is
        // when the defaults were centralized here; unifying it would
        // change real behaviour and is a separate decision.
        default_timeout_secs: 60,
        default_max_retries: 2,
        requires_pdf_native_feature: false,
    },
    ProtocolSpec {
        name: "dots-ocr",
        mode: ModeKind::ModelProtocol,
        shape: ProtocolShape::OneShotPage,
        transport: TransportContract::OpenAiChatCompletions,
        preprocess: PreprocessKind::SmartResize,
        decode: DecodeKind::StrictJson,
        coordinates: CoordinateKind::PixelAbs,
        order: OrderSource::FromModel,
        default_endpoint: Some("http://localhost:8000/v1/chat/completions"),
        default_model: Some("model"),
        default_timeout_secs: 120,
        default_max_retries: 2,
        requires_pdf_native_feature: false,
    },
    ProtocolSpec {
        name: "generic-vlm",
        mode: ModeKind::ModelProtocol,
        shape: ProtocolShape::OneShotPage,
        transport: TransportContract::OpenAiChatCompletions,
        preprocess: PreprocessKind::PageImage,
        decode: DecodeKind::Markdown,
        coordinates: CoordinateKind::None,
        order: OrderSource::FromModel,
        default_endpoint: Some("http://localhost:8000/v1/chat/completions"),
        default_model: Some("model"),
        default_timeout_secs: 120,
        default_max_retries: 2,
        requires_pdf_native_feature: false,
    },
    ProtocolSpec {
        name: "monkeyocr-v2",
        mode: ModeKind::ModelProtocol,
        shape: ProtocolShape::LayoutThenRecognize,
        transport: TransportContract::OpenAiChatCompletions,
        preprocess: PreprocessKind::PixelBounds,
        decode: DecodeKind::PythonLiteral,
        coordinates: CoordinateKind::Norm0To1000,
        order: OrderSource::FromModel,
        default_endpoint: Some("http://localhost:8888/v1/chat/completions"),
        default_model: Some("monkeyocrv2"),
        default_timeout_secs: 120,
        default_max_retries: 2,
        requires_pdf_native_feature: false,
    },
    ProtocolSpec {
        name: "navidc-ocr",
        mode: ModeKind::ModelProtocol,
        shape: ProtocolShape::LayoutThenRecognize,
        transport: TransportContract::OpenAiChatCompletions,
        preprocess: PreprocessKind::HardResize,
        decode: DecodeKind::NaviLayoutTokens,
        coordinates: CoordinateKind::Norm0To1000,
        order: OrderSource::FromModel,
        default_endpoint: Some("http://localhost:8000/v1/chat/completions"),
        default_model: Some("StarDoc-AI/NaviDC-OCR"),
        default_timeout_secs: 120,
        default_max_retries: 2,
        requires_pdf_native_feature: false,
    },
    ProtocolSpec {
        name: "paddleocr",
        mode: ModeKind::ModelProtocol,
        shape: ProtocolShape::StructuredService,
        transport: TransportContract::PaddleOcrService,
        preprocess: PreprocessKind::PageImage,
        decode: DecodeKind::OcrBoxes,
        coordinates: CoordinateKind::PixelAbs,
        order: OrderSource::GeometricFallback,
        default_endpoint: Some("http://localhost:8868/predict/ocr_system"),
        // This service contract has no model field at all — a configured
        // `model` is reported as ignored rather than silently dropped.
        default_model: None,
        // 60s, unlike the 120s its sibling `paddlex-structure` uses.
        // Preserved as-is; see the `mineru-vlm` note.
        default_timeout_secs: 60,
        default_max_retries: 2,
        requires_pdf_native_feature: false,
    },
    ProtocolSpec {
        name: "paddlex-structure",
        mode: ModeKind::ModelProtocol,
        shape: ProtocolShape::StructuredService,
        transport: TransportContract::PaddleOcrService,
        preprocess: PreprocessKind::PageImage,
        decode: DecodeKind::StructuredEnvelope,
        coordinates: CoordinateKind::None,
        order: OrderSource::FromModel,
        default_endpoint: Some("http://localhost:8080/layout-parsing"),
        default_model: None,
        default_timeout_secs: 120,
        default_max_retries: 2,
        requires_pdf_native_feature: false,
    },
    ProtocolSpec {
        name: "pipeline",
        mode: ModeKind::Pipeline,
        shape: ProtocolShape::StageGraph,
        transport: TransportContract::PipelineStages,
        preprocess: PreprocessKind::StageGraph,
        decode: DecodeKind::StageOutputs,
        coordinates: CoordinateKind::PixelAbs,
        order: OrderSource::AdapterComputed,
        // Unlike every other entry this is a *base* URL; the nine stage
        // endpoints are derived from it by
        // `PipelineV2Adapter::from_endpoint_base`. It used to be `None`
        // here, which is why the literal was duplicated in
        // `pipeline_v2.rs` and `cli.rs`'s doctor branch.
        default_endpoint: Some("http://localhost:9001"),
        default_model: None,
        // 180s: a stage-graph request does far more work per call.
        default_timeout_secs: 180,
        default_max_retries: 2,
        requires_pdf_native_feature: false,
    },
    ProtocolSpec {
        name: "mock",
        mode: ModeKind::Test,
        shape: ProtocolShape::Mock,
        transport: TransportContract::None,
        preprocess: PreprocessKind::None,
        decode: DecodeKind::Mock,
        coordinates: CoordinateKind::PixelAbs,
        order: OrderSource::FromModel,
        default_endpoint: None,
        default_model: None,
        default_timeout_secs: 0,
        default_max_retries: 0,
        requires_pdf_native_feature: false,
    },
];

pub fn get(name: &str) -> Option<&'static ProtocolSpec> {
    PROTOCOL_SPECS.iter().find(|spec| spec.name == name)
}

/// Infallible lookup for adapter `Default` impls. Every adapter passes its
/// own compile-time name constant, and
/// `names_are_unique_and_cover_registered_adapters` asserts each registered
/// adapter has a spec — so a miss here is a programming error at authoring
/// time, not a runtime condition a caller could handle.
pub fn spec_of(name: &'static str) -> &'static ProtocolSpec {
    get(name).unwrap_or_else(|| panic!("no ProtocolSpec declared for protocol {name:?}"))
}

impl ProtocolSpec {
    /// The built-in endpoint as an owned `String`, for adapters whose field
    /// is a `String`. Panics for a protocol declaring no endpoint — those
    /// adapters (`native`/`tesseract`/`mock`) have no endpoint field to
    /// fill, so reaching this would itself be the bug.
    pub fn endpoint_default(&self) -> String {
        self.default_endpoint
            .unwrap_or_else(|| panic!("protocol {:?} declares no default endpoint", self.name))
            .to_owned()
    }

    pub fn model_default(&self) -> String {
        self.default_model
            .unwrap_or_else(|| panic!("protocol {:?} declares no default model", self.name))
            .to_owned()
    }

    pub fn timeout_default(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.default_timeout_secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_cover_registered_adapters() {
        let mut names: Vec<_> = PROTOCOL_SPECS.iter().map(|spec| spec.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), PROTOCOL_SPECS.len());

        let registry = crate::adapters::Registry::with_builtins();
        for name in registry.names() {
            assert!(get(name).is_some(), "missing ProtocolSpec for {name}");
            let adapter = registry
                .build(name, &crate::adapters::AdapterOverrides::default())
                .expect("registered adapter can be built");
            assert_eq!(adapter.spec().name, name);
            let expected_coordinates = match adapter.spec().coordinates {
                CoordinateKind::Norm0To1000 => crate::types::CoordinateSystem::Norm0To1000,
                CoordinateKind::SourceSemantic
                | CoordinateKind::PixelAbs
                | CoordinateKind::FullPage
                | CoordinateKind::None => crate::types::CoordinateSystem::PixelAbs,
            };
            assert_eq!(adapter.coordinate_system(), expected_coordinates);
            assert_eq!(
                adapter.provides_reading_order(),
                adapter.spec().order != OrderSource::GeometricFallback
            );
        }
    }

    /// Every adapter's `Default` impl now *reads* its endpoint/model/
    /// timeout/retries from the spec, so this can no longer catch a drifted
    /// copy — there is only one copy. What it still earns its keep for is
    /// catching a spec entry that declares nothing (or the wrong thing) for
    /// an adapter that does need it, and it now covers `pipeline`, which
    /// the previous endpoint-only version silently omitted.
    #[test]
    fn adapter_defaults_come_from_the_spec() {
        use crate::adapters::*;

        macro_rules! check {
            ($name:literal, $adapter:expr, $endpoint:ident) => {{
                let spec = get($name).unwrap();
                let adapter = $adapter;
                assert_eq!(
                    spec.default_endpoint,
                    Some(adapter.$endpoint.as_str()),
                    "{} endpoint",
                    $name
                );
                assert_eq!(spec.timeout_default(), adapter.timeout, "{} timeout", $name);
                assert_eq!(
                    spec.default_max_retries, adapter.max_retries,
                    "{} max_retries",
                    $name
                );
                adapter
            }};
        }

        let a = check!(
            "mineru-vlm",
            mineru_vlm::MineruVlmAdapter::default(),
            endpoint_base
        );
        assert_eq!(get("mineru-vlm").unwrap().default_model.unwrap(), a.model);
        let a = check!(
            "dots-ocr",
            dots_ocr::DotsOcrAdapter::default(),
            endpoint_base
        );
        assert_eq!(get("dots-ocr").unwrap().default_model.unwrap(), a.model);
        let a = check!(
            "generic-vlm",
            generic_vlm::GenericVlmAdapter::default(),
            endpoint
        );
        assert_eq!(get("generic-vlm").unwrap().default_model.unwrap(), a.model);
        let a = check!(
            "monkeyocr-v2",
            monkeyocr_v2::MonkeyOcrV2Adapter::default(),
            endpoint_base
        );
        assert_eq!(get("monkeyocr-v2").unwrap().default_model.unwrap(), a.model);
        let a = check!(
            "navidc-ocr",
            navidc_ocr::NavidcOcrAdapter::default(),
            endpoint_base
        );
        assert_eq!(get("navidc-ocr").unwrap().default_model.unwrap(), a.model);

        // These three have no model field at all; the spec says so.
        check!(
            "paddleocr",
            paddleocr::PaddleOcrAdapter::default(),
            endpoint
        );
        assert!(get("paddleocr").unwrap().default_model.is_none());
        check!(
            "paddlex-structure",
            paddlex_structure::PaddleXStructureAdapter::default(),
            endpoint
        );
        assert!(get("paddlex-structure").unwrap().default_model.is_none());
        check!(
            "pipeline",
            pipeline_v2::PipelineV2Adapter::default(),
            endpoint_base
        );
        assert!(get("pipeline").unwrap().default_model.is_none());
    }

    /// `pipeline`'s base URL used to live in three places at once
    /// (`pipeline_v2.rs`, `cli.rs`'s doctor branch, and a `None` here), with
    /// no test able to catch drift between them. The spec is now the source
    /// and every stage URL is derived from it.
    #[test]
    fn pipeline_stage_endpoints_all_derive_from_the_spec_base() {
        let base = get("pipeline").unwrap().default_endpoint.unwrap();
        let adapter = crate::adapters::pipeline_v2::PipelineV2Adapter::default();
        for stage in [
            &adapter.layout_endpoint,
            &adapter.formula_detection_endpoint,
            &adapter.ocr_endpoint,
            &adapter.formula_recognition_endpoint,
            &adapter.table_endpoint,
        ] {
            assert!(
                stage.starts_with(base),
                "stage endpoint {stage:?} does not derive from spec base {base:?}"
            );
        }
    }
}
