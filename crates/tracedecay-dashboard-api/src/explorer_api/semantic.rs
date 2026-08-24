//! Typed semantic-provider state for the Explorer coordinator.
//!
//! The semantic lane never executes retrieval from this surface: authorized
//! semantic search requires the authenticated query composition authority,
//! which is not mounted at the dashboard HTTP boundary. What this surface can
//! do truthfully is consult the daemon's semantic activation gate and runtime
//! status and report the provider's real typed state — unactivated is a typed
//! absence, model acquisition and vector projection are `indexing`, and a
//! generation mismatch is `stale` — instead of leaving the provider invisible.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use tracedecay_usecases::semantic_runtime::{
    SemanticFallbackReasonV1, SemanticRuntimeStateV1, SemanticRuntimeStatusV1,
};

use super::{DashboardCoverageV1, ExplorerSourceIdV1, ExplorerSourceProgressV1};
use crate::DashboardState;

/// One daemon-observed reading of the semantic provider for a project root.
///
/// `activated` is the durable activation gate (committed semantic
/// compatibility pins exist for the scope); `status` is the mounted runtime's
/// cheap application status, absent when no project semantic runtime is
/// registered in the daemon process.
#[derive(Clone, Debug)]
pub struct ExplorerSemanticReadV1 {
    pub activated: bool,
    pub status: Option<SemanticRuntimeStatusV1>,
}

pub type ExplorerSemanticReadFuture =
    Pin<Box<dyn Future<Output = ExplorerSemanticReadV1> + Send + 'static>>;
/// Daemon-owned reader over the semantic activation gate and runtime status.
/// Standalone dashboards leave it absent and the semantic source reports a
/// typed `unsupported` instead of guessing from process-local state.
pub type ExplorerSemanticReader =
    Arc<dyn Fn(PathBuf) -> ExplorerSemanticReadFuture + Send + Sync + 'static>;

pub(super) async fn semantic_source(state: &DashboardState) -> ExplorerSourceProgressV1 {
    let Some(reader) = state.explorer_semantic_reader.as_ref() else {
        return ExplorerSourceProgressV1::unsupported(
            ExplorerSourceIdV1::Semantic,
            "semantic_status_unattached",
            "the dashboard is not attached to a daemon-owned semantic runtime authority",
        );
    };
    semantic_source_from_read(reader(state.project_root.clone()).await)
}

/// Map one daemon reading to the semantic source's wire outcome.
///
/// The activation gate is checked first: without committed pins there is no
/// vector index for this scope and never was, so the source is `absent` with
/// the complete accounting of an empty domain — not `unavailable`, which
/// would claim an existing store failed to answer.
pub(super) fn semantic_source_from_read(read: ExplorerSemanticReadV1) -> ExplorerSourceProgressV1 {
    if !read.activated {
        return ExplorerSourceProgressV1::absent(
            ExplorerSourceIdV1::Semantic,
            "semantic_not_activated",
            "semantic search is not activated for this project; no vector index exists until an accepted semantic profile is activated",
            "indexed vectors",
        );
    }
    let Some(status) = read.status else {
        return ExplorerSourceProgressV1::unavailable(
            ExplorerSourceIdV1::Semantic,
            "semantic_runtime_unavailable",
            "the committed semantic activation has no mounted runtime in the daemon",
        );
    };
    match status.state {
        SemanticRuntimeStateV1::Unavailable { reason } => {
            semantic_reason_source(reason, "the semantic runtime cannot serve this scope")
        }
        SemanticRuntimeStateV1::SelectedNotDownloaded { model_id, .. } => {
            ExplorerSourceProgressV1::indexing(
                ExplorerSourceIdV1::Semantic,
                "semantic_model_not_downloaded",
                format!("selected model {model_id} has not been downloaded yet"),
            )
        }
        SemanticRuntimeStateV1::Downloading {
            model_id,
            bytes_received,
            bytes_total,
            ..
        } => {
            let mut source = ExplorerSourceProgressV1::indexing(
                ExplorerSourceIdV1::Semantic,
                "semantic_model_downloading",
                format!("downloading model {model_id}"),
            );
            source.completed_units = Some(bytes_received);
            source.total_units = Some(bytes_total);
            source
        }
        SemanticRuntimeStateV1::Verifying { model_id, .. } => ExplorerSourceProgressV1::indexing(
            ExplorerSourceIdV1::Semantic,
            "semantic_model_verifying",
            format!("verifying downloaded model {model_id}"),
        ),
        SemanticRuntimeStateV1::Installed { model_id, .. } => ExplorerSourceProgressV1::indexing(
            ExplorerSourceIdV1::Semantic,
            "semantic_model_installed",
            format!("model {model_id} is installed and awaiting load"),
        ),
        SemanticRuntimeStateV1::Loading { model_id, .. } => ExplorerSourceProgressV1::indexing(
            ExplorerSourceIdV1::Semantic,
            "semantic_model_loading",
            format!("loading model {model_id} into the embedding runtime"),
        ),
        SemanticRuntimeStateV1::Indexing {
            completed_units,
            total_units,
        } => {
            let mut source = ExplorerSourceProgressV1::indexing(
                ExplorerSourceIdV1::Semantic,
                "semantic_indexing",
                "semantic vector projection is in progress",
            );
            source.completed_units = Some(completed_units);
            source.total_units = Some(total_units);
            source.coverage = DashboardCoverageV1::partial(
                total_units,
                completed_units,
                "semantic units",
                vec!["semantic vector projection is in progress".to_owned()],
            );
            source
        }
        SemanticRuntimeStateV1::Current { .. } => ExplorerSourceProgressV1::unsupported(
            ExplorerSourceIdV1::Semantic,
            "semantic_execution_unmounted",
            "the semantic index is current, but explorer cannot execute semantic retrieval on this surface",
        ),
        SemanticRuntimeStateV1::Degraded { reason, .. } => {
            semantic_reason_source(reason, "the semantic runtime is degraded")
        }
        SemanticRuntimeStateV1::Rollback {
            from_generation: _,
            target_generation: _,
        } => ExplorerSourceProgressV1::indexing(
            ExplorerSourceIdV1::Semantic,
            "semantic_rollback",
            "a semantic generation rollback is in progress",
        ),
        SemanticRuntimeStateV1::Failed { detail, .. } => {
            ExplorerSourceProgressV1::error(ExplorerSourceIdV1::Semantic, "semantic_failed", detail)
        }
    }
}

/// One outcome per fallback reason, so an in-progress acquisition never reads
/// as a failure and a stale generation never reads as a missing runtime.
fn semantic_reason_source(
    reason: SemanticFallbackReasonV1,
    context: &'static str,
) -> ExplorerSourceProgressV1 {
    let code = semantic_reason_code(reason);
    let message = format!("{context}: {code}");
    match reason {
        SemanticFallbackReasonV1::Stale => {
            ExplorerSourceProgressV1::stale(ExplorerSourceIdV1::Semantic, code, message)
        }
        SemanticFallbackReasonV1::Indexing
        | SemanticFallbackReasonV1::RollbackInProgress
        | SemanticFallbackReasonV1::SelectedNotDownloaded
        | SemanticFallbackReasonV1::Downloading
        | SemanticFallbackReasonV1::Verifying
        | SemanticFallbackReasonV1::Loading => {
            ExplorerSourceProgressV1::indexing(ExplorerSourceIdV1::Semantic, code, message)
        }
        SemanticFallbackReasonV1::RuntimeFailure | SemanticFallbackReasonV1::ModelFailed => {
            ExplorerSourceProgressV1::error(ExplorerSourceIdV1::Semantic, code, message)
        }
        SemanticFallbackReasonV1::ConfigurationUnavailable
        | SemanticFallbackReasonV1::RuntimeUnavailable
        | SemanticFallbackReasonV1::ArtifactUnavailable
        | SemanticFallbackReasonV1::IncompatibleRuntime
        | SemanticFallbackReasonV1::ResourceCeilingExceeded
        | SemanticFallbackReasonV1::CorruptArtifact
        | SemanticFallbackReasonV1::InvalidRuntimeStatus
        | SemanticFallbackReasonV1::ResetRequired => {
            ExplorerSourceProgressV1::unavailable(ExplorerSourceIdV1::Semantic, code, message)
        }
    }
}

const fn semantic_reason_code(reason: SemanticFallbackReasonV1) -> &'static str {
    match reason {
        SemanticFallbackReasonV1::ConfigurationUnavailable => "semantic_configuration_unavailable",
        SemanticFallbackReasonV1::RuntimeUnavailable => "semantic_runtime_unavailable",
        SemanticFallbackReasonV1::ArtifactUnavailable => "semantic_artifact_unavailable",
        SemanticFallbackReasonV1::IncompatibleRuntime => "semantic_incompatible_runtime",
        SemanticFallbackReasonV1::ResourceCeilingExceeded => "semantic_resource_ceiling_exceeded",
        SemanticFallbackReasonV1::CorruptArtifact => "semantic_corrupt_artifact",
        SemanticFallbackReasonV1::Indexing => "semantic_indexing",
        SemanticFallbackReasonV1::RuntimeFailure => "semantic_runtime_failure",
        SemanticFallbackReasonV1::RollbackInProgress => "semantic_rollback",
        SemanticFallbackReasonV1::InvalidRuntimeStatus => "semantic_invalid_runtime_status",
        SemanticFallbackReasonV1::SelectedNotDownloaded => "semantic_model_not_downloaded",
        SemanticFallbackReasonV1::Downloading => "semantic_model_downloading",
        SemanticFallbackReasonV1::Verifying => "semantic_model_verifying",
        SemanticFallbackReasonV1::Loading => "semantic_model_loading",
        SemanticFallbackReasonV1::ModelFailed => "semantic_failed",
        SemanticFallbackReasonV1::Stale => "semantic_generation_stale",
        SemanticFallbackReasonV1::ResetRequired => "semantic_reset_required",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explorer_api::{ExplorerSourceOutcomeV1, ExplorerSourcePhaseV1};

    fn read(activated: bool, state: Option<SemanticRuntimeStateV1>) -> ExplorerSemanticReadV1 {
        ExplorerSemanticReadV1 {
            activated,
            status: state.map(|state| SemanticRuntimeStatusV1::new(None, state)),
        }
    }

    #[test]
    fn unactivated_semantic_is_typed_absent_with_complete_zero_accounting() {
        let source = semantic_source_from_read(read(false, None));

        assert_eq!(source.source_id, ExplorerSourceIdV1::Semantic);
        assert_eq!(source.outcome, ExplorerSourceOutcomeV1::Absent);
        assert_eq!(source.phase, ExplorerSourcePhaseV1::Completed);
        assert_eq!(source.error_code, Some("semantic_not_activated"));
        assert!(source.coverage.is_complete());
        assert_eq!(source.completed_units, Some(0));
        assert_eq!(source.total_units, Some(0));
    }

    #[test]
    fn unactivated_wins_over_a_present_runtime_status() {
        let source = semantic_source_from_read(read(
            false,
            Some(SemanticRuntimeStateV1::Indexing {
                completed_units: 1,
                total_units: 2,
            }),
        ));

        assert_eq!(source.outcome, ExplorerSourceOutcomeV1::Absent);
    }

    #[test]
    fn activated_without_a_mounted_runtime_is_unavailable_not_absent() {
        let source = semantic_source_from_read(read(true, None));

        assert_eq!(source.outcome, ExplorerSourceOutcomeV1::Unavailable);
        assert_eq!(source.error_code, Some("semantic_runtime_unavailable"));
        assert!(!source.coverage.is_complete());
    }

    #[test]
    fn vector_projection_reports_indexing_with_progress_units() {
        let source = semantic_source_from_read(read(
            true,
            Some(SemanticRuntimeStateV1::Indexing {
                completed_units: 3,
                total_units: 10,
            }),
        ));

        assert_eq!(source.outcome, ExplorerSourceOutcomeV1::Indexing);
        assert_eq!(source.error_code, Some("semantic_indexing"));
        assert_eq!(source.completed_units, Some(3));
        assert_eq!(source.total_units, Some(10));
        assert!(!source.coverage.is_complete());
    }

    #[test]
    fn model_acquisition_reports_indexing_with_the_exact_stage() {
        let source = semantic_source_from_read(read(
            true,
            Some(SemanticRuntimeStateV1::Downloading {
                model_id: "model.fixture".to_owned(),
                artifact_digest: "sha256:fixture".to_owned(),
                bytes_received: 5,
                bytes_total: 100,
            }),
        ));

        assert_eq!(source.outcome, ExplorerSourceOutcomeV1::Indexing);
        assert_eq!(source.error_code, Some("semantic_model_downloading"));
        assert_eq!(source.completed_units, Some(5));
        assert_eq!(source.total_units, Some(100));
    }

    #[test]
    fn stale_generation_reports_stale_not_unavailable() {
        let source = semantic_source_from_read(read(
            true,
            Some(SemanticRuntimeStateV1::Degraded {
                active_generation: None,
                reason: SemanticFallbackReasonV1::Stale,
            }),
        ));

        assert_eq!(source.outcome, ExplorerSourceOutcomeV1::Stale);
        assert_eq!(source.error_code, Some("semantic_generation_stale"));
    }

    #[test]
    fn degraded_runtime_failure_is_an_error_with_the_reason_code() {
        let source = semantic_source_from_read(read(
            true,
            Some(SemanticRuntimeStateV1::Degraded {
                active_generation: None,
                reason: SemanticFallbackReasonV1::RuntimeFailure,
            }),
        ));

        assert_eq!(source.outcome, ExplorerSourceOutcomeV1::Error);
        assert_eq!(source.error_code, Some("semantic_runtime_failure"));
    }

    #[test]
    fn failed_runtime_carries_the_failure_detail() {
        let source = semantic_source_from_read(read(
            true,
            Some(SemanticRuntimeStateV1::Failed {
                model_id: "model.fixture".to_owned(),
                artifact_digest: "sha256:fixture".to_owned(),
                detail: "projection worker crashed".to_owned(),
                retryable: true,
            }),
        ));

        assert_eq!(source.outcome, ExplorerSourceOutcomeV1::Error);
        assert_eq!(source.error_code, Some("semantic_failed"));
        assert_eq!(source.message.as_deref(), Some("projection worker crashed"));
    }

    #[test]
    fn rollback_in_progress_reports_indexing() {
        let source = semantic_source_from_read(read(
            true,
            Some(SemanticRuntimeStateV1::Unavailable {
                reason: SemanticFallbackReasonV1::RollbackInProgress,
            }),
        ));

        assert_eq!(source.outcome, ExplorerSourceOutcomeV1::Indexing);
        assert_eq!(source.error_code, Some("semantic_rollback"));
    }
}
