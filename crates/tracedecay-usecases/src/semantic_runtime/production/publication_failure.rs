use std::sync::{Arc, Mutex};

use tracedecay_semantic::SemanticRuntimeScheduleFailureV1;

use crate::store::vector_generations::VectorGenerationStoreErrorV1;

use super::super::graph_provider::SemanticVectorGraphErrorV1;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SemanticPublicationStageV1 {
    RetainForResume,
    OpenStore,
    ConfigureStage,
    BeginGeneration,
    RetainForBatch,
    CommitBatch,
    RetainForPublish,
    PublishGeneration,
}

impl SemanticPublicationStageV1 {
    #[cfg(test)]
    pub(super) const ALL: [Self; 8] = [
        Self::RetainForResume,
        Self::OpenStore,
        Self::ConfigureStage,
        Self::BeginGeneration,
        Self::RetainForBatch,
        Self::CommitBatch,
        Self::RetainForPublish,
        Self::PublishGeneration,
    ];

    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::RetainForResume => "retain_for_resume",
            Self::OpenStore => "open_store",
            Self::ConfigureStage => "configure_stage",
            Self::BeginGeneration => "begin_generation",
            Self::RetainForBatch => "retain_for_batch",
            Self::CommitBatch => "commit_batch",
            Self::RetainForPublish => "retain_for_publish",
            Self::PublishGeneration => "publish_generation",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SemanticPublicationFailureCategoryV1 {
    GraphUnavailable,
    GraphRejected,
    StoreCancelled,
    StoreDeadlineExceeded,
    StoreResetRequired,
    StoreCorrupt,
    StoreUnavailable,
    StoreDurabilityUncertain,
    StoreInvalidPlan,
    StoreUnknownBuild,
    StoreStaleCheckpoint,
    StoreBatchIdentityMismatch,
    StoreConflictingBatchReplay,
    StoreDuplicateChunkEffect,
    StoreIncompatibleBaseGeneration,
    StoreMissingBaseVector,
    StoreMissingAppliedVector,
    StoreIncompleteGeneration,
    StoreImmutableGenerationConflict,
    StorePhysicalVectorConflict,
    StoreStorage,
    StoreConcurrentMutation,
    StoreProjection,
    MissingBuildState,
    MissingStoreState,
}

impl SemanticPublicationFailureCategoryV1 {
    #[cfg(test)]
    pub(super) const ALL: [Self; 25] = [
        Self::GraphUnavailable,
        Self::GraphRejected,
        Self::StoreCancelled,
        Self::StoreDeadlineExceeded,
        Self::StoreResetRequired,
        Self::StoreCorrupt,
        Self::StoreUnavailable,
        Self::StoreDurabilityUncertain,
        Self::StoreInvalidPlan,
        Self::StoreUnknownBuild,
        Self::StoreStaleCheckpoint,
        Self::StoreBatchIdentityMismatch,
        Self::StoreConflictingBatchReplay,
        Self::StoreDuplicateChunkEffect,
        Self::StoreIncompatibleBaseGeneration,
        Self::StoreMissingBaseVector,
        Self::StoreMissingAppliedVector,
        Self::StoreIncompleteGeneration,
        Self::StoreImmutableGenerationConflict,
        Self::StorePhysicalVectorConflict,
        Self::StoreStorage,
        Self::StoreConcurrentMutation,
        Self::StoreProjection,
        Self::MissingBuildState,
        Self::MissingStoreState,
    ];

    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::GraphUnavailable => "graph_unavailable",
            Self::GraphRejected => "graph_rejected",
            Self::StoreCancelled => "store_cancelled",
            Self::StoreDeadlineExceeded => "store_deadline_exceeded",
            Self::StoreResetRequired => "store_reset_required",
            Self::StoreCorrupt => "store_corrupt",
            Self::StoreUnavailable => "store_unavailable",
            Self::StoreDurabilityUncertain => "store_durability_uncertain",
            Self::StoreInvalidPlan => "store_invalid_plan",
            Self::StoreUnknownBuild => "store_unknown_build",
            Self::StoreStaleCheckpoint => "store_stale_checkpoint",
            Self::StoreBatchIdentityMismatch => "store_batch_identity_mismatch",
            Self::StoreConflictingBatchReplay => "store_conflicting_batch_replay",
            Self::StoreDuplicateChunkEffect => "store_duplicate_chunk_effect",
            Self::StoreIncompatibleBaseGeneration => "store_incompatible_base_generation",
            Self::StoreMissingBaseVector => "store_missing_base_vector",
            Self::StoreMissingAppliedVector => "store_missing_applied_vector",
            Self::StoreIncompleteGeneration => "store_incomplete_generation",
            Self::StoreImmutableGenerationConflict => "store_immutable_generation_conflict",
            Self::StorePhysicalVectorConflict => "store_physical_vector_conflict",
            Self::StoreStorage => "store_storage",
            Self::StoreConcurrentMutation => "store_concurrent_mutation",
            Self::StoreProjection => "store_projection",
            Self::MissingBuildState => "missing_build_state",
            Self::MissingStoreState => "missing_store_state",
        }
    }

    fn from_graph(error: &SemanticVectorGraphErrorV1) -> Self {
        match error {
            SemanticVectorGraphErrorV1::Unavailable(_) => Self::GraphUnavailable,
            SemanticVectorGraphErrorV1::Rejected(_) => Self::GraphRejected,
        }
    }

    fn from_store(error: &VectorGenerationStoreErrorV1) -> Self {
        match error {
            VectorGenerationStoreErrorV1::Cancelled => Self::StoreCancelled,
            VectorGenerationStoreErrorV1::DeadlineExceeded => Self::StoreDeadlineExceeded,
            VectorGenerationStoreErrorV1::ResetRequired(_) => Self::StoreResetRequired,
            VectorGenerationStoreErrorV1::Corrupt(_) => Self::StoreCorrupt,
            VectorGenerationStoreErrorV1::Unavailable(_) => Self::StoreUnavailable,
            VectorGenerationStoreErrorV1::DurabilityUncertain(_) => Self::StoreDurabilityUncertain,
            VectorGenerationStoreErrorV1::InvalidPlan(_) => Self::StoreInvalidPlan,
            VectorGenerationStoreErrorV1::UnknownBuild => Self::StoreUnknownBuild,
            VectorGenerationStoreErrorV1::StaleCheckpoint => Self::StoreStaleCheckpoint,
            VectorGenerationStoreErrorV1::BatchIdentityMismatch => Self::StoreBatchIdentityMismatch,
            VectorGenerationStoreErrorV1::ConflictingBatchReplay => {
                Self::StoreConflictingBatchReplay
            }
            VectorGenerationStoreErrorV1::DuplicateChunkEffect(_) => {
                Self::StoreDuplicateChunkEffect
            }
            VectorGenerationStoreErrorV1::IncompatibleBaseGeneration(_) => {
                Self::StoreIncompatibleBaseGeneration
            }
            VectorGenerationStoreErrorV1::MissingBaseVector(_) => Self::StoreMissingBaseVector,
            VectorGenerationStoreErrorV1::MissingAppliedVector(_) => {
                Self::StoreMissingAppliedVector
            }
            VectorGenerationStoreErrorV1::IncompleteGeneration => Self::StoreIncompleteGeneration,
            VectorGenerationStoreErrorV1::ImmutableGenerationConflict => {
                Self::StoreImmutableGenerationConflict
            }
            VectorGenerationStoreErrorV1::PhysicalVectorConflict => {
                Self::StorePhysicalVectorConflict
            }
            VectorGenerationStoreErrorV1::Storage(_) => Self::StoreStorage,
            VectorGenerationStoreErrorV1::ConcurrentMutation => Self::StoreConcurrentMutation,
            VectorGenerationStoreErrorV1::Projection(_) => Self::StoreProjection,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SemanticPublicationFailureReceiptV1 {
    pub(super) stage: SemanticPublicationStageV1,
    pub(super) category: SemanticPublicationFailureCategoryV1,
}

impl SemanticPublicationFailureReceiptV1 {
    pub(super) fn detail(self) -> String {
        format!(
            "semantic runtime publication {} failed: {}",
            self.stage.as_str(),
            self.category.as_str()
        )
    }
}

#[derive(Clone, Default)]
pub(super) struct SemanticPublicationFailureRecorderV1 {
    first: Arc<Mutex<Option<SemanticPublicationFailureReceiptV1>>>,
}

impl SemanticPublicationFailureRecorderV1 {
    pub(super) fn retain_for_resume(
        &self,
        error: &SemanticVectorGraphErrorV1,
    ) -> SemanticRuntimeScheduleFailureV1 {
        self.record_graph(SemanticPublicationStageV1::RetainForResume, error)
    }

    pub(super) fn open_store(
        &self,
        error: &VectorGenerationStoreErrorV1,
    ) -> SemanticRuntimeScheduleFailureV1 {
        self.record_store(SemanticPublicationStageV1::OpenStore, error)
    }

    pub(super) fn configure_stage(
        &self,
        error: &VectorGenerationStoreErrorV1,
    ) -> SemanticRuntimeScheduleFailureV1 {
        self.record_store(SemanticPublicationStageV1::ConfigureStage, error)
    }

    pub(super) fn begin_generation(
        &self,
        error: &VectorGenerationStoreErrorV1,
    ) -> SemanticRuntimeScheduleFailureV1 {
        self.record_store(SemanticPublicationStageV1::BeginGeneration, error)
    }

    pub(super) fn missing_commit_build(&self) -> SemanticRuntimeScheduleFailureV1 {
        self.record_internal(
            SemanticPublicationStageV1::CommitBatch,
            SemanticPublicationFailureCategoryV1::MissingBuildState,
        )
    }

    pub(super) fn missing_commit_store(&self) -> SemanticRuntimeScheduleFailureV1 {
        self.record_internal(
            SemanticPublicationStageV1::CommitBatch,
            SemanticPublicationFailureCategoryV1::MissingStoreState,
        )
    }

    pub(super) fn retain_for_batch(
        &self,
        error: &SemanticVectorGraphErrorV1,
    ) -> SemanticRuntimeScheduleFailureV1 {
        self.record_graph(SemanticPublicationStageV1::RetainForBatch, error)
    }

    pub(super) fn commit_batch(
        &self,
        error: &VectorGenerationStoreErrorV1,
    ) -> SemanticRuntimeScheduleFailureV1 {
        self.record_store(SemanticPublicationStageV1::CommitBatch, error)
    }

    pub(super) fn missing_publish_build(&self) -> SemanticRuntimeScheduleFailureV1 {
        self.record_internal(
            SemanticPublicationStageV1::PublishGeneration,
            SemanticPublicationFailureCategoryV1::MissingBuildState,
        )
    }

    pub(super) fn missing_publish_store(&self) -> SemanticRuntimeScheduleFailureV1 {
        self.record_internal(
            SemanticPublicationStageV1::PublishGeneration,
            SemanticPublicationFailureCategoryV1::MissingStoreState,
        )
    }

    pub(super) fn retain_for_publish(
        &self,
        error: &SemanticVectorGraphErrorV1,
    ) -> SemanticRuntimeScheduleFailureV1 {
        self.record_graph(SemanticPublicationStageV1::RetainForPublish, error)
    }

    pub(super) fn publish_generation(
        &self,
        error: &VectorGenerationStoreErrorV1,
    ) -> SemanticRuntimeScheduleFailureV1 {
        self.record_store(SemanticPublicationStageV1::PublishGeneration, error)
    }

    pub(super) fn record_graph(
        &self,
        stage: SemanticPublicationStageV1,
        error: &SemanticVectorGraphErrorV1,
    ) -> SemanticRuntimeScheduleFailureV1 {
        self.record(
            stage,
            SemanticPublicationFailureCategoryV1::from_graph(error),
        )
    }

    pub(super) fn record_store(
        &self,
        stage: SemanticPublicationStageV1,
        error: &VectorGenerationStoreErrorV1,
    ) -> SemanticRuntimeScheduleFailureV1 {
        self.record(
            stage,
            SemanticPublicationFailureCategoryV1::from_store(error),
        )
    }

    pub(super) fn record_internal(
        &self,
        stage: SemanticPublicationStageV1,
        category: SemanticPublicationFailureCategoryV1,
    ) -> SemanticRuntimeScheduleFailureV1 {
        self.record(stage, category)
    }

    pub(super) fn receipt(&self) -> Option<SemanticPublicationFailureReceiptV1> {
        *self
            .first
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn record(
        &self,
        stage: SemanticPublicationStageV1,
        category: SemanticPublicationFailureCategoryV1,
    ) -> SemanticRuntimeScheduleFailureV1 {
        let mut first = self
            .first
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if first.is_none() {
            tracing::warn!(
                event = "semantic_publication_failure",
                stage = stage.as_str(),
                category = category.as_str(),
                "semantic publication failed at a bounded production stage"
            );
            *first = Some(SemanticPublicationFailureReceiptV1 { stage, category });
        }
        SemanticRuntimeScheduleFailureV1::Publication
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use crate::store::vector_generations::VectorGenerationStoreErrorV1;

    use super::{
        SemanticPublicationFailureCategoryV1, SemanticPublicationFailureRecorderV1,
        SemanticPublicationStageV1,
    };
    use crate::semantic_runtime::SemanticVectorGraphErrorV1;

    #[test]
    fn publication_failure_receipts_keep_bounded_stage_and_category_truth() {
        let stage_labels = SemanticPublicationStageV1::ALL
            .iter()
            .map(|stage| stage.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(stage_labels.len(), SemanticPublicationStageV1::ALL.len());

        let category_labels = SemanticPublicationFailureCategoryV1::ALL
            .iter()
            .map(|category| category.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            category_labels.len(),
            SemanticPublicationFailureCategoryV1::ALL.len()
        );

        let unavailable = SemanticPublicationFailureRecorderV1::default();
        unavailable.record_graph(
            SemanticPublicationStageV1::RetainForResume,
            &SemanticVectorGraphErrorV1::Unavailable("backend path is private".to_owned()),
        );
        assert_eq!(
            unavailable
                .receipt()
                .expect("graph failure receipt")
                .category,
            SemanticPublicationFailureCategoryV1::GraphUnavailable
        );

        let rejected = SemanticPublicationFailureRecorderV1::default();
        rejected.record_graph(
            SemanticPublicationStageV1::RetainForResume,
            &SemanticVectorGraphErrorV1::Rejected("backend path is private".to_owned()),
        );
        assert_eq!(
            rejected.receipt().expect("graph failure receipt").category,
            SemanticPublicationFailureCategoryV1::GraphRejected
        );

        for (error, expected) in [
            (
                VectorGenerationStoreErrorV1::Cancelled,
                SemanticPublicationFailureCategoryV1::StoreCancelled,
            ),
            (
                VectorGenerationStoreErrorV1::DeadlineExceeded,
                SemanticPublicationFailureCategoryV1::StoreDeadlineExceeded,
            ),
            (
                VectorGenerationStoreErrorV1::InvalidPlan("private detail".to_owned()),
                SemanticPublicationFailureCategoryV1::StoreInvalidPlan,
            ),
            (
                VectorGenerationStoreErrorV1::ConcurrentMutation,
                SemanticPublicationFailureCategoryV1::StoreConcurrentMutation,
            ),
        ] {
            let recorder = SemanticPublicationFailureRecorderV1::default();
            recorder.record_store(SemanticPublicationStageV1::BeginGeneration, &error);
            assert_eq!(
                recorder.receipt().expect("store failure receipt").category,
                expected
            );
        }

        let first = SemanticPublicationFailureRecorderV1::default();
        first.record_internal(
            SemanticPublicationStageV1::CommitBatch,
            SemanticPublicationFailureCategoryV1::MissingBuildState,
        );
        first.record_internal(
            SemanticPublicationStageV1::PublishGeneration,
            SemanticPublicationFailureCategoryV1::MissingStoreState,
        );
        let receipt = first.receipt().expect("first failure receipt");
        assert_eq!(receipt.stage, SemanticPublicationStageV1::CommitBatch);
        assert_eq!(
            receipt.category,
            SemanticPublicationFailureCategoryV1::MissingBuildState
        );
        assert_eq!(
            receipt.detail(),
            "semantic runtime publication commit_batch failed: missing_build_state"
        );
    }
}
