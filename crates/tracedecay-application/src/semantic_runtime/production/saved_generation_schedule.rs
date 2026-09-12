//! Saved code generation scheduling hook installed by the composition root.

use std::path::PathBuf;
use std::sync::Arc;

use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_domain::{EmbeddingDocumentCompositionV1, WorktreeId};
use tracedecay_semantic::{DaemonSemanticRuntimeHandleV1, SemanticModelLifecycleOwnerV1};
use tracedecay_semantic_contracts::{SemanticResourceCeilings, SemanticRuntimeScheduleFailureV1};

use super::super::graph_provider::SemanticVectorGraphProviderV1;
use super::super::ports::SemanticRuntimeBackendErrorV1;
use super::super::{
    DaemonGlobalSemanticProjectionSchedulerV1, SemanticProjectionBatchV1,
    SemanticProjectionScheduleErrorV1,
};
use super::ProductionSemanticRuntimeV1;
use super::evaluation_support::resolved_resident_ceiling;
use super::project_registry::project_semantic_production_runtimes;

/// Why a published code generation did or did not enter semantic projection.
///
/// Every decline was previously a bare `false` that each caller discarded, so
/// a daemon whose runtime stopped scheduling sat at `installed` with no record
/// of why nothing was queued (#753). The runtime's own declines are already
/// named by `semantic_projection_schedule`; these are the handoff-boundary
/// reasons that never reach it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SavedGenerationScheduleOutcomeV1 {
    /// Semantic projection was queued for this generation.
    Scheduled,
    /// No semantic runtime is mounted on this scheduler — never mounted, or
    /// retired by a remount — so no hook observed the generation at all.
    RuntimeNotMounted,
    /// The generation belongs to a different worktree than the mounted runtime.
    ForeignWorktree,
    /// The hook was built outside a Tokio runtime, so projection has no
    /// executor to dispatch onto.
    NoDispatchRuntime,
    /// The fair projection scheduler refused the batch (queue capacity,
    /// cancellation); `semantic_projection_schedule` carries the detail.
    QueueRefused,
    /// The hook panicked; the generation remains serving.
    HookPanicked,
    /// The code-index scheduler itself could not be reached: the worktree is
    /// not mounted, or it is shutting down.
    SchedulerUnavailable,
    /// The mounted worktree has not sealed a serving generation yet, so there
    /// is nothing to offer.
    NoServingGeneration,
}

impl SavedGenerationScheduleOutcomeV1 {
    #[must_use]
    pub fn is_scheduled(self) -> bool {
        matches!(self, Self::Scheduled)
    }

    /// Fixed, privacy-safe classification for the diagnostic record.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scheduled => "scheduled",
            Self::RuntimeNotMounted => "runtime_not_mounted",
            Self::ForeignWorktree => "foreign_worktree",
            Self::NoDispatchRuntime => "no_dispatch_runtime",
            Self::QueueRefused => "queue_refused",
            Self::HookPanicked => "hook_panicked",
            Self::SchedulerUnavailable => "scheduler_unavailable",
            Self::NoServingGeneration => "no_serving_generation",
        }
    }
}

/// Hook invoked after a code generation publishes; must not block search.
///
/// The serving owner transfers a shared handle because one decoded generation
/// can be much larger than its captured source. Semantic retention and queued
/// projection must clone this `Arc`, never the immutable generation payload.
pub type SavedCodeGenerationScheduleHookV1 = Arc<
    dyn Fn(Arc<CodeIndexPublishedGenerationV1>) -> SavedGenerationScheduleOutcomeV1 + Send + Sync,
>;

/// Owned authorities and identities captured by a saved-generation hook.
pub struct SavedGenerationScheduleHookParametersV1 {
    pub project_root: PathBuf,
    pub code_index_store_root: PathBuf,
    pub worktree_id: WorktreeId,
    pub handle: DaemonSemanticRuntimeHandleV1,
    /// Daemon-implemented resolution to the Grafeo code-graph runtime that
    /// owns the durable semantic-vector projection.
    pub graph: Arc<dyn SemanticVectorGraphProviderV1>,
    pub lifecycle: Arc<SemanticModelLifecycleOwnerV1>,
    pub resources: SemanticResourceCeilings,
    pub document_composition: EmbeddingDocumentCompositionV1,
    pub fair_scheduler: DaemonGlobalSemanticProjectionSchedulerV1,
}

/// Production hook: enqueue semantic projection for each saved generation.
///
/// Artifact admission remains owned by the model lifecycle. Until a complete
/// compatible artifact is available the background task fails closed without
/// joining into exact/lexical/graph search.
///
/// The resident ceiling is resolved once here rather than per generation: it
/// is a composition-time fact, and an unresolved one is a typed refusal to
/// build the hook at all instead of a number invented per batch.
pub fn production_saved_generation_schedule_hook(
    parameters: SavedGenerationScheduleHookParametersV1,
) -> Result<SavedCodeGenerationScheduleHookV1, SemanticRuntimeBackendErrorV1> {
    let SavedGenerationScheduleHookParametersV1 {
        project_root,
        code_index_store_root,
        worktree_id,
        handle,
        graph,
        lifecycle,
        resources,
        document_composition,
        fair_scheduler,
    } = parameters;
    let resident_ceiling_bytes = resolved_resident_ceiling(resources)?;
    let runtime = Arc::new(ProductionSemanticRuntimeV1::new_with_code_index_store_root(
        handle,
        graph,
        code_index_store_root,
        lifecycle,
        resources,
        document_composition,
    ));
    project_semantic_production_runtimes()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(project_root.clone(), runtime.as_ref().clone());
    // Capture before the synchronous publication hook crosses into spawn_blocking.
    let dispatch_runtime = tokio::runtime::Handle::try_current().ok();
    Ok(Arc::new(move |generation| {
        if generation.snapshot().worktree.as_ref() != Some(&worktree_id) {
            return SavedGenerationScheduleOutcomeV1::ForeignWorktree;
        }
        super::super::register_project_semantic_redundancy_generation(
            project_root.clone(),
            Arc::clone(&generation),
        );
        let runtime = Arc::clone(&runtime);
        let Some(dispatch_runtime) = dispatch_runtime.clone() else {
            return SavedGenerationScheduleOutcomeV1::NoDispatchRuntime;
        };
        let queued_bytes = generation
            .chunks()
            .chunks()
            .iter()
            .fold(0_u64, |total, chunk| {
                total.saturating_add(
                    u64::try_from(chunk.sanitized_text.as_str().len()).unwrap_or(u64::MAX),
                )
            });
        let batch = SemanticProjectionBatchV1::new(
            worktree_id.clone(),
            generation.manifest().generation_id.clone(),
            queued_bytes,
            resident_ceiling_bytes,
        );
        let project_root = project_root.clone();
        fair_scheduler
            .enqueue_work(
                batch,
                Box::new(move |lease| {
                    dispatch_runtime.spawn(async move {
                        let lease = Arc::new(lease);
                        if lease.is_cancelled() {
                            return;
                        }
                        let Ok(lease) = Arc::try_unwrap(lease) else {
                            return;
                        };
                        if let Some(required) =
                            super::super::project_committed_semantic_pins(&project_root)
                            && matches!(
                                runtime
                                    .restore_current(
                                        generation.manifest(),
                                        &required.vector_generation_id
                                    )
                                    .await,
                                Ok(true)
                            )
                        {
                            return;
                        }
                        let _ = runtime.schedule_saved_generation_fair(generation, lease);
                    });
                }),
            )
            .inspect_err(|error| {
                tracing::warn!(
                    event = "semantic_projection_schedule",
                    outcome = "enqueue_failed",
                    error = ?error,
                    "semantic projection could not be queued for this code generation"
                );
            })
            .map_or(SavedGenerationScheduleOutcomeV1::QueueRefused, |_| {
                SavedGenerationScheduleOutcomeV1::Scheduled
            })
    }))
}

pub(super) fn fair_schedule_failure(
    error: SemanticProjectionScheduleErrorV1,
) -> SemanticRuntimeScheduleFailureV1 {
    match error {
        SemanticProjectionScheduleErrorV1::Cancelled => SemanticRuntimeScheduleFailureV1::Cancelled,
        SemanticProjectionScheduleErrorV1::QueueBytesCapacity { .. }
        | SemanticProjectionScheduleErrorV1::QueueBatchCapacity { .. }
        | SemanticProjectionScheduleErrorV1::SessionMemoryReservationTooLarge { .. }
        | SemanticProjectionScheduleErrorV1::PublicationCapacity { .. }
        | SemanticProjectionScheduleErrorV1::PublicationAlreadyClaimed => {
            SemanticRuntimeScheduleFailureV1::Publication
        }
    }
}
