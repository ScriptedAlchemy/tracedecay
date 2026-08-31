use std::fmt;

use serde::Serialize;
use tracedecay_domain::{CodeGenerationId, ProjectionKeyV1, VectorGenerationIdV1};

use crate::configuration::SemanticFallbackReasonV1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticGenerationPointerV1 {
    pub generation: VectorGenerationIdV1,
    pub source_generation: CodeGenerationId,
    pub projection_key: ProjectionKeyV1,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SemanticRuntimeScheduleFailureV1 {
    Artifact,
    ArtifactDetail(String),
    Runtime,
    Projection,
    ProjectionDetail(String),
    Publication,
    PublicationDetail(String),
    Cancelled,
    DeadlineExceeded,
}

impl SemanticRuntimeScheduleFailureV1 {
    pub fn artifact(error: impl fmt::Display) -> Self {
        Self::ArtifactDetail(error.to_string())
    }

    pub fn projection(error: impl fmt::Display) -> Self {
        Self::ProjectionDetail(error.to_string())
    }

    pub fn is_projection(&self) -> bool {
        matches!(self, Self::Projection | Self::ProjectionDetail(_))
    }

    pub fn publication(error: impl fmt::Display) -> Self {
        Self::PublicationDetail(error.to_string())
    }

    pub fn is_publication(&self) -> bool {
        matches!(self, Self::Publication | Self::PublicationDetail(_))
    }
}

impl fmt::Display for SemanticRuntimeScheduleFailureV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Artifact => formatter.write_str("Artifact"),
            Self::ArtifactDetail(detail) => write!(formatter, "Artifact: {detail}"),
            Self::Runtime => formatter.write_str("Runtime"),
            Self::Projection => formatter.write_str("Projection"),
            Self::ProjectionDetail(detail) => write!(formatter, "Projection: {detail}"),
            Self::Publication => formatter.write_str("Publication"),
            Self::PublicationDetail(detail) => write!(formatter, "Publication: {detail}"),
            Self::Cancelled => formatter.write_str("Cancelled"),
            Self::DeadlineExceeded => formatter.write_str("DeadlineExceeded"),
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SemanticRuntimeScheduleStatusV1 {
    Unavailable,
    Indexing {
        target_generation: CodeGenerationId,
        target_projection_key: Option<ProjectionKeyV1>,
        completed_units: u64,
        total_units: u64,
        prior_generation: Option<VectorGenerationIdV1>,
    },
    Current {
        generation: VectorGenerationIdV1,
    },
    Failed {
        reason: SemanticRuntimeScheduleFailureV1,
        prior_generation: Option<VectorGenerationIdV1>,
    },
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct SemanticRuntimeStatusProjectionV1 {
    pub status: SemanticRuntimeScheduleStatusV1,
    pub degraded_reason: Option<SemanticFallbackReasonV1>,
    pub prior_generation: Option<VectorGenerationIdV1>,
}
