use crate::DocumentFormat;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A terminal failure that prevented meaningful structured output.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DocumentError {
    #[error("unsupported document format: {0:?}")]
    UnsupportedFormat(DocumentFormat),
    #[error("malformed document: {detail}")]
    Malformed {
        part: Option<String>,
        detail: String,
    },
    #[error("document is encrypted or password-protected")]
    Encrypted,
    #[error("resource limit exceeded ({limit}): {detail}")]
    ResourceLimit { limit: &'static str, detail: String },
    #[error("missing required part: {part}")]
    MissingPart { part: String },
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl DocumentError {
    pub(crate) fn malformed(detail: impl Into<String>) -> Self {
        Self::Malformed {
            part: None,
            detail: detail.into(),
        }
    }
}

/// Stable category for a recoverable parse warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarningCode {
    OptionalPartSkipped,
    BrokenRelationship,
    UnsupportedFeature,
    StyleCycle,
    TruncatedContent,
    InvalidSpanClamped,
    AssetDropped,
}

/// A recoverable source issue retained in the output contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParseWarning {
    pub code: WarningCode,
    pub part: Option<String>,
    pub message: String,
}
