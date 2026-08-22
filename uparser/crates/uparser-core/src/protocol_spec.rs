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
    Mock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoordinateKind {
    SourceSemantic,
    Norm0To1000,
    PixelAbs,
    FullPage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderSource {
    SourceSemantic,
    FromModel,
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
    pub default_endpoint: Option<&'static str>,
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
        default_endpoint: None,
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
        default_endpoint: None,
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
        requires_pdf_native_feature: false,
    },
    ProtocolSpec {
        name: "generic-vlm",
        mode: ModeKind::ModelProtocol,
        shape: ProtocolShape::OneShotPage,
        transport: TransportContract::OpenAiChatCompletions,
        preprocess: PreprocessKind::PageImage,
        decode: DecodeKind::Markdown,
        coordinates: CoordinateKind::FullPage,
        order: OrderSource::FromModel,
        default_endpoint: Some("http://localhost:8000/v1/chat/completions"),
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
        requires_pdf_native_feature: false,
    },
    ProtocolSpec {
        name: "paddlex-structure",
        mode: ModeKind::ModelProtocol,
        shape: ProtocolShape::StructuredService,
        transport: TransportContract::PaddleOcrService,
        preprocess: PreprocessKind::PageImage,
        decode: DecodeKind::StructuredEnvelope,
        coordinates: CoordinateKind::FullPage,
        order: OrderSource::FromModel,
        default_endpoint: Some("http://localhost:8080/layout-parsing"),
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
        order: OrderSource::GeometricFallback,
        default_endpoint: None,
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
        requires_pdf_native_feature: false,
    },
];

pub fn get(name: &str) -> Option<&'static ProtocolSpec> {
    PROTOCOL_SPECS.iter().find(|spec| spec.name == name)
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
                | CoordinateKind::FullPage => crate::types::CoordinateSystem::PixelAbs,
            };
            assert_eq!(adapter.coordinate_system(), expected_coordinates);
            assert_eq!(
                adapter.provides_reading_order(),
                adapter.spec().order != OrderSource::GeometricFallback
            );
        }
    }

    #[test]
    fn declared_default_endpoints_match_adapter_defaults() {
        assert_eq!(
            get("mineru-vlm").unwrap().default_endpoint,
            Some(
                crate::adapters::mineru_vlm::MineruVlmAdapter::default()
                    .endpoint_base
                    .as_str()
            )
        );
        assert_eq!(
            get("dots-ocr").unwrap().default_endpoint,
            Some(
                crate::adapters::dots_ocr::DotsOcrAdapter::default()
                    .endpoint_base
                    .as_str()
            )
        );
        assert_eq!(
            get("generic-vlm").unwrap().default_endpoint,
            Some(
                crate::adapters::generic_vlm::GenericVlmAdapter::default()
                    .endpoint
                    .as_str()
            )
        );
        assert_eq!(
            get("monkeyocr-v2").unwrap().default_endpoint,
            Some(
                crate::adapters::monkeyocr_v2::MonkeyOcrV2Adapter::default()
                    .endpoint_base
                    .as_str()
            )
        );
        assert_eq!(
            get("paddleocr").unwrap().default_endpoint,
            Some(
                crate::adapters::paddleocr::PaddleOcrAdapter::default()
                    .endpoint
                    .as_str()
            )
        );
        assert_eq!(
            get("paddlex-structure").unwrap().default_endpoint,
            Some(
                crate::adapters::paddlex_structure::PaddleXStructureAdapter::default()
                    .endpoint
                    .as_str()
            )
        );
    }
}
