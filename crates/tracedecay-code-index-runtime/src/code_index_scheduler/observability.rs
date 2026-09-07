//! Canonical Plan 26 observability lane for one mounted code-index worktree.
//! Telemetry never changes the product path: refusals are logged and dropped,
//! and an uninstalled lane records nothing.

use std::sync::Arc;

use tracedecay_domain::{
    CoverageStateV1, IndexObservationKindV1, IndexObservedV1, IndexOutcomeV1, QueueDepthBucketV1,
    RetrievalBudget,
};
use tracedecay_query::retrieval::AuthorizedQueryFallbackV1;
use tracedecay_query::retrieval::observation::observe_composition;
use tracedecay_usecases::observability::{
    BoundedObservabilityProducerV1, ObservabilityEmissionOutcomeV1, emit_index,
    emit_retrieval_pipeline,
};

use super::CodeIndexReconcileOutcomeV1;

/// Project-bound observation authority installed once per mounted worktree
/// (`CodeIndexSchedulerRegistryV1::install_index_observability`). The session
/// bounded producer carries lifecycle and retrieval-pipeline observations off
/// the scheduler and query hot paths.
#[derive(Clone)]
pub struct CodeIndexObservabilityV1 {
    producer: Arc<BoundedObservabilityProducerV1>,
}

impl CodeIndexObservabilityV1 {
    pub fn new(producer: Arc<BoundedObservabilityProducerV1>) -> Self {
        Self { producer }
    }

    /// Records one terminal reconcile pass as a canonical index lifecycle
    /// observation beside the worker's in-memory cadence receipt.
    pub fn record_reconcile_outcome(
        &self,
        outcome: &CodeIndexReconcileOutcomeV1,
        service_micros: u64,
        queue_depth_bucket: QueueDepthBucketV1,
    ) {
        let observation = reconcile_index_observation(outcome, service_micros, queue_depth_bucket);
        match emit_index(self.producer.as_ref(), observation) {
            Ok(ObservabilityEmissionOutcomeV1::Enqueued) => {}
            Ok(ObservabilityEmissionOutcomeV1::DroppedAtCapacity) => tracing::debug!(
                event = "code_index_observability",
                family = "index",
                outcome = "dropped_at_capacity",
                "code-index lifecycle observation was refused by the bounded producer"
            ),
            Err(error) => tracing::debug!(
                event = "code_index_observability",
                family = "index",
                outcome = "unavailable",
                error,
                "code-index lifecycle observation could not be enqueued"
            ),
        }
    }

    /// Offers the retrieval-pipeline families projected from one completed
    /// query composition to the bounded producer, non-blocking on the query
    /// hot path.
    pub fn record_retrieval_composition(
        &self,
        authorized: &AuthorizedQueryFallbackV1,
        budget: &RetrievalBudget,
    ) {
        // Tokens are countable only after hydration; the projection reports
        // partial synthesis coverage rather than a fabricated zero.
        let observation = observe_composition(
            &authorized.fallback_lanes,
            &authorized.composition,
            budget,
            None,
        );
        let summary = emit_retrieval_pipeline(
            self.producer.as_ref(),
            self.producer.identity(),
            observation,
        );
        if summary.dropped > 0 || summary.invalid > 0 {
            tracing::debug!(
                event = "code_index_observability",
                family = "retrieval_pipeline",
                enqueued = summary.enqueued,
                dropped = summary.dropped,
                invalid = summary.invalid,
                "retrieval-pipeline observations were partially refused by the bounded producer"
            );
        }
    }
}

/// Project one terminal reconcile outcome into the closed index-lifecycle
/// vocabulary. A publication carries its changed-chunk volume; a no-op rescan
/// produced no items and abstains rather than counting as a publication.
fn reconcile_index_observation(
    outcome: &CodeIndexReconcileOutcomeV1,
    service_micros: u64,
    queue_depth_bucket: QueueDepthBucketV1,
) -> IndexObservedV1 {
    match outcome {
        CodeIndexReconcileOutcomeV1::Published(evidence) => IndexObservedV1 {
            kind: IndexObservationKindV1::Publication,
            duration_micros: Some(service_micros),
            item_count: Some(evidence.changed_chunks as u64),
            queue_depth_bucket,
            outcome: IndexOutcomeV1::Published,
            // The worker fully observed this pass from wake to seal.
            coverage: CoverageStateV1::Known,
        },
        CodeIndexReconcileOutcomeV1::Noop(_) => IndexObservedV1 {
            kind: IndexObservationKindV1::Rescan,
            duration_micros: Some(service_micros),
            item_count: None,
            queue_depth_bucket,
            outcome: IndexOutcomeV1::NoOp,
            coverage: CoverageStateV1::Known,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{
        CodeGenerationId, ContentDigest, ManifestDigest, ObservabilityPayloadV1, RepositoryId,
    };

    fn published() -> CodeIndexReconcileOutcomeV1 {
        CodeIndexReconcileOutcomeV1::Published(super::super::CodeIndexPublishEvidenceV1 {
            generation_id: CodeGenerationId::new("generation.observability.fixture")
                .expect("generation id"),
            repository_id: RepositoryId::new("repository.observability.fixture")
                .expect("repository id"),
            snapshot_content_identity: ContentDigest::new(format!("sha256:{}", "a".repeat(64)))
                .expect("content digest"),
            lane_digest: ManifestDigest::new(format!("sha256:{}", "b".repeat(64)))
                .expect("lane digest"),
            file_occurrence_ids: Vec::new(),
            reextracted_files: 3,
            changed_chunks: 7,
            reused_chunks: 11,
            overflow_reconciled: false,
        })
    }

    #[test]
    fn a_publication_projects_as_a_published_lifecycle_observation() {
        let observation = reconcile_index_observation(&published(), 900, QueueDepthBucketV1::Zero);
        assert_eq!(observation.kind, IndexObservationKindV1::Publication);
        assert_eq!(observation.outcome, IndexOutcomeV1::Published);
        assert_eq!(observation.duration_micros, Some(900));
        assert_eq!(observation.item_count, Some(7));
        ObservabilityPayloadV1::Index(observation)
            .validate()
            .expect("publication observation validates");
    }

    #[test]
    fn a_noop_rescan_abstains_instead_of_counting_as_a_publication() {
        let outcome = CodeIndexReconcileOutcomeV1::Noop(super::super::CodeIndexNoopEvidenceV1 {
            snapshot_content_identity: ContentDigest::new(format!("sha256:{}", "c".repeat(64)))
                .expect("content digest"),
            overflow_reconciled: false,
        });
        let observation =
            reconcile_index_observation(&outcome, 250, QueueDepthBucketV1::OneToEight);
        assert_eq!(observation.kind, IndexObservationKindV1::Rescan);
        assert_eq!(observation.outcome, IndexOutcomeV1::NoOp);
        assert_eq!(observation.item_count, None);
        ObservabilityPayloadV1::Index(observation)
            .validate()
            .expect("no-op observation validates");
    }
}
