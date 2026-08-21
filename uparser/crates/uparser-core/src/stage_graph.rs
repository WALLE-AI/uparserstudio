//! Typed dependency graph resolution for Mode 3 pipeline execution.

use serde::Serialize;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StageKind {
    Preprocess,
    Layout,
    Formula,
    Ocr,
    Table,
    Seal,
    Chart,
    Assemble,
    Order,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StageData {
    PageImage,
    Regions,
    RecognizedRegions,
    OrderedBlocks,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailurePolicy {
    AbortPage,
    IsolateRegion,
    ContinueWithoutStage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OcrInput {
    NotApplicable,
    External,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StageNode {
    pub name: &'static str,
    pub kind: StageKind,
    pub enabled: bool,
    pub depends_on: &'static [&'static str],
    pub accepts: &'static [StageData],
    pub produces: StageData,
    pub on_failure: FailurePolicy,
    pub ocr_input: OcrInput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StageGraph {
    pub source: StageData,
    pub nodes: &'static [StageNode],
    pub required: &'static [StageKind],
    pub terminal: StageData,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum StageGraphError {
    #[error("duplicate stage: {0}")]
    Duplicate(String),
    #[error("required stage kind is missing or disabled: {0:?}")]
    MissingRequired(StageKind),
    #[error("stage {stage} depends on missing or disabled stage {dependency}")]
    MissingDependency { stage: String, dependency: String },
    #[error("stage graph contains a cycle involving {0}")]
    Cycle(String),
    #[error("stage {stage} cannot consume {input:?} from {dependency}")]
    IncompatibleInput {
        stage: String,
        dependency: String,
        input: StageData,
    },
    #[error("table stage {0} declares external OCR but does not depend on an OCR stage")]
    MissingExternalOcr(String),
    #[error(
        "stage {stage} may be skipped on failure but is required by downstream stage {dependent}"
    )]
    SkippableDependency { stage: String, dependent: String },
    #[error("stage graph does not produce terminal data {0:?}")]
    MissingTerminal(StageData),
}

impl StageGraph {
    pub fn validate(&self) -> Result<(), StageGraphError> {
        self.resolve().map(|_| ())
    }

    pub fn resolve(&self) -> Result<Vec<&StageNode>, StageGraphError> {
        let mut by_name = HashMap::new();
        for node in self.nodes {
            if by_name.insert(node.name, node).is_some() {
                return Err(StageGraphError::Duplicate(node.name.to_owned()));
            }
        }
        let enabled: HashMap<_, _> = by_name
            .iter()
            .filter(|(_, node)| node.enabled)
            .map(|(name, node)| (*name, *node))
            .collect();

        for kind in self.required {
            if !enabled.values().any(|node| node.kind == *kind) {
                return Err(StageGraphError::MissingRequired(*kind));
            }
        }

        for node in enabled.values() {
            if node.depends_on.is_empty() && !node.accepts.contains(&self.source) {
                return Err(StageGraphError::IncompatibleInput {
                    stage: node.name.to_owned(),
                    dependency: "source".to_owned(),
                    input: self.source,
                });
            }
            for dependency in node.depends_on {
                let producer =
                    enabled
                        .get(dependency)
                        .ok_or_else(|| StageGraphError::MissingDependency {
                            stage: node.name.to_owned(),
                            dependency: (*dependency).to_owned(),
                        })?;
                if !node.accepts.contains(&producer.produces) {
                    return Err(StageGraphError::IncompatibleInput {
                        stage: node.name.to_owned(),
                        dependency: (*dependency).to_owned(),
                        input: producer.produces,
                    });
                }
            }
            if node.kind == StageKind::Table
                && node.ocr_input == OcrInput::External
                && !node.depends_on.iter().any(|dependency| {
                    enabled
                        .get(dependency)
                        .is_some_and(|producer| producer.kind == StageKind::Ocr)
                })
            {
                return Err(StageGraphError::MissingExternalOcr(node.name.to_owned()));
            }
            if node.on_failure == FailurePolicy::ContinueWithoutStage {
                if let Some(dependent) = enabled
                    .values()
                    .find(|dependent| dependent.depends_on.contains(&node.name))
                {
                    return Err(StageGraphError::SkippableDependency {
                        stage: node.name.to_owned(),
                        dependent: dependent.name.to_owned(),
                    });
                }
            }
        }
        if !enabled.values().any(|node| node.produces == self.terminal) {
            return Err(StageGraphError::MissingTerminal(self.terminal));
        }

        let mut visiting = HashSet::new();
        let mut visited = HashSet::new();
        let mut ordered = Vec::with_capacity(enabled.len());
        for node in self.nodes.iter().filter(|node| node.enabled) {
            visit(
                node.name,
                &enabled,
                &mut visiting,
                &mut visited,
                &mut ordered,
            )?;
        }
        Ok(ordered)
    }
}

fn visit<'a>(
    name: &'a str,
    by_name: &HashMap<&'a str, &'a StageNode>,
    visiting: &mut HashSet<&'a str>,
    visited: &mut HashSet<&'a str>,
    ordered: &mut Vec<&'a StageNode>,
) -> Result<(), StageGraphError> {
    if visited.contains(name) {
        return Ok(());
    }
    if !visiting.insert(name) {
        return Err(StageGraphError::Cycle(name.to_owned()));
    }
    let node = by_name[name];
    for dependency in node.depends_on {
        visit(*dependency, by_name, visiting, visited, ordered)?;
    }
    visiting.remove(name);
    visited.insert(name);
    ordered.push(node);
    Ok(())
}

const PIPELINE_NODES: &[StageNode] = &[
    StageNode {
        name: "layout",
        kind: StageKind::Layout,
        enabled: true,
        depends_on: &[],
        accepts: &[StageData::PageImage],
        produces: StageData::Regions,
        on_failure: FailurePolicy::AbortPage,
        ocr_input: OcrInput::NotApplicable,
    },
    StageNode {
        name: "formula",
        kind: StageKind::Formula,
        enabled: true,
        depends_on: &["layout"],
        accepts: &[StageData::Regions],
        produces: StageData::RecognizedRegions,
        on_failure: FailurePolicy::IsolateRegion,
        ocr_input: OcrInput::NotApplicable,
    },
    StageNode {
        name: "ocr",
        kind: StageKind::Ocr,
        enabled: true,
        depends_on: &["layout"],
        accepts: &[StageData::Regions],
        produces: StageData::RecognizedRegions,
        on_failure: FailurePolicy::IsolateRegion,
        ocr_input: OcrInput::NotApplicable,
    },
    StageNode {
        name: "table",
        kind: StageKind::Table,
        enabled: true,
        depends_on: &["layout"],
        accepts: &[StageData::Regions],
        produces: StageData::RecognizedRegions,
        on_failure: FailurePolicy::IsolateRegion,
        ocr_input: OcrInput::Internal,
    },
    StageNode {
        name: "assemble",
        kind: StageKind::Assemble,
        enabled: true,
        depends_on: &["layout", "formula", "ocr", "table"],
        accepts: &[StageData::Regions, StageData::RecognizedRegions],
        produces: StageData::OrderedBlocks,
        on_failure: FailurePolicy::AbortPage,
        ocr_input: OcrInput::NotApplicable,
    },
    StageNode {
        name: "order",
        kind: StageKind::Order,
        enabled: true,
        depends_on: &["assemble"],
        accepts: &[StageData::OrderedBlocks],
        produces: StageData::OrderedBlocks,
        on_failure: FailurePolicy::AbortPage,
        ocr_input: OcrInput::NotApplicable,
    },
];

pub const PIPELINE_STAGE_GRAPH: StageGraph = StageGraph {
    source: StageData::PageImage,
    nodes: PIPELINE_NODES,
    required: &[
        StageKind::Layout,
        StageKind::Ocr,
        StageKind::Assemble,
        StageKind::Order,
    ],
    terminal: StageData::OrderedBlocks,
};

#[cfg(test)]
mod tests {
    use super::*;

    const BASE_NODE: StageNode = StageNode {
        name: "layout",
        kind: StageKind::Layout,
        enabled: true,
        depends_on: &[],
        accepts: &[StageData::PageImage],
        produces: StageData::Regions,
        on_failure: FailurePolicy::AbortPage,
        ocr_input: OcrInput::NotApplicable,
    };

    fn graph(nodes: &'static [StageNode]) -> StageGraph {
        StageGraph {
            source: StageData::PageImage,
            nodes,
            required: &[],
            terminal: StageData::Regions,
        }
    }

    #[test]
    fn pipeline_graph_resolves_dependencies_before_consumers() {
        let resolved = PIPELINE_STAGE_GRAPH.resolve().unwrap();
        let position = |name| resolved.iter().position(|node| node.name == name).unwrap();
        assert!(position("layout") < position("ocr"));
        assert!(position("ocr") < position("assemble"));
        assert!(position("formula") < position("assemble"));
        assert!(position("table") < position("assemble"));
        assert!(position("assemble") < position("order"));
    }

    #[test]
    fn missing_or_disabled_required_stage_is_rejected() {
        let graph = StageGraph {
            required: &[StageKind::Ocr],
            ..graph(&[BASE_NODE])
        };
        assert_eq!(
            graph.validate(),
            Err(StageGraphError::MissingRequired(StageKind::Ocr))
        );
    }

    #[test]
    fn missing_dependency_is_rejected() {
        const NODES: &[StageNode] = &[StageNode {
            name: "ocr",
            kind: StageKind::Ocr,
            enabled: true,
            depends_on: &["layout"],
            accepts: &[StageData::Regions],
            produces: StageData::RecognizedRegions,
            on_failure: FailurePolicy::AbortPage,
            ocr_input: OcrInput::NotApplicable,
        }];
        assert!(matches!(
            graph(NODES).validate(),
            Err(StageGraphError::MissingDependency { .. })
        ));
    }

    #[test]
    fn external_table_ocr_requires_an_ocr_dependency() {
        const NODES: &[StageNode] = &[
            BASE_NODE,
            StageNode {
                name: "table",
                kind: StageKind::Table,
                enabled: true,
                depends_on: &["layout"],
                accepts: &[StageData::Regions],
                produces: StageData::RecognizedRegions,
                on_failure: FailurePolicy::IsolateRegion,
                ocr_input: OcrInput::External,
            },
        ];
        assert!(matches!(
            graph(NODES).validate(),
            Err(StageGraphError::MissingExternalOcr(_))
        ));
    }

    #[test]
    fn downstream_cannot_require_a_stage_that_may_be_skipped() {
        const NODES: &[StageNode] = &[
            StageNode {
                on_failure: FailurePolicy::ContinueWithoutStage,
                ..BASE_NODE
            },
            StageNode {
                name: "ocr",
                kind: StageKind::Ocr,
                enabled: true,
                depends_on: &["layout"],
                accepts: &[StageData::Regions],
                produces: StageData::Regions,
                on_failure: FailurePolicy::AbortPage,
                ocr_input: OcrInput::NotApplicable,
            },
        ];
        assert_eq!(
            graph(NODES).validate(),
            Err(StageGraphError::SkippableDependency {
                stage: "layout".to_owned(),
                dependent: "ocr".to_owned(),
            })
        );
    }

    #[test]
    fn cycle_is_rejected() {
        const NODES: &[StageNode] = &[
            StageNode {
                name: "a",
                kind: StageKind::Layout,
                enabled: true,
                depends_on: &["b"],
                accepts: &[StageData::Regions],
                produces: StageData::Regions,
                on_failure: FailurePolicy::AbortPage,
                ocr_input: OcrInput::NotApplicable,
            },
            StageNode {
                name: "b",
                kind: StageKind::Ocr,
                enabled: true,
                depends_on: &["a"],
                accepts: &[StageData::Regions],
                produces: StageData::Regions,
                on_failure: FailurePolicy::AbortPage,
                ocr_input: OcrInput::NotApplicable,
            },
        ];
        assert!(matches!(
            graph(NODES).validate(),
            Err(StageGraphError::Cycle(_))
        ));
    }
}
