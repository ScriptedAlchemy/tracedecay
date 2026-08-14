use tracedecay_semantic::{
    SemanticRuntimeScheduleFailureV1, SemanticRuntimeScheduleStatusV1,
    SemanticRuntimeStatusProjectionV1,
};

use super::super::ports::{
    SemanticConfigurationPinV1, SemanticFallbackReasonV1, SemanticRuntimeStateV1,
    SemanticRuntimeStatusV1,
};

/// Map daemon schedule projection into the application/Doctor status shape.
///
/// Indexing never blocks exact/lexical/graph; the route remains lexical until
/// [`SemanticRuntimeStateV1::Current`].
pub fn application_status_from_projection(
    projection: &SemanticRuntimeStatusProjectionV1,
    configuration: Option<SemanticConfigurationPinV1>,
) -> SemanticRuntimeStatusV1 {
    let state = match &projection.status {
        SemanticRuntimeScheduleStatusV1::Unavailable => SemanticRuntimeStateV1::Unavailable {
            reason: projection
                .degraded_reason
                .unwrap_or(SemanticFallbackReasonV1::RuntimeUnavailable),
        },
        SemanticRuntimeScheduleStatusV1::Indexing {
            completed_units,
            total_units,
            ..
        } => SemanticRuntimeStateV1::Indexing {
            completed_units: *completed_units,
            total_units: *total_units,
        },
        SemanticRuntimeScheduleStatusV1::Failed {
            reason,
            prior_generation,
        } => SemanticRuntimeStateV1::Degraded {
            active_generation: prior_generation
                .clone()
                .or_else(|| projection.prior_generation.clone()),
            reason: match reason {
                SemanticRuntimeScheduleFailureV1::Artifact => {
                    SemanticFallbackReasonV1::ArtifactUnavailable
                }
                SemanticRuntimeScheduleFailureV1::Cancelled
                | SemanticRuntimeScheduleFailureV1::DeadlineExceeded => {
                    SemanticFallbackReasonV1::RuntimeUnavailable
                }
                SemanticRuntimeScheduleFailureV1::Runtime
                | SemanticRuntimeScheduleFailureV1::Projection
                | SemanticRuntimeScheduleFailureV1::ProjectionDetail(_)
                | SemanticRuntimeScheduleFailureV1::Publication
                | SemanticRuntimeScheduleFailureV1::PublicationDetail(_) => {
                    SemanticFallbackReasonV1::RuntimeFailure
                }
            },
        },
        SemanticRuntimeScheduleStatusV1::Current { generation } => {
            SemanticRuntimeStateV1::Degraded {
                active_generation: Some(generation.clone()),
                reason: SemanticFallbackReasonV1::InvalidRuntimeStatus,
            }
        }
    };
    SemanticRuntimeStatusV1::new(configuration, state)
}
