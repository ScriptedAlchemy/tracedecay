//! Canonical Plan 26 observability lane for one mounted code-index worktree.
//! Telemetry never changes the product path: refusals are logged and dropped,
//! and an uninstalled lane records nothing.

use std::sync::Arc;

use tracedecay_application::observability::{
    BoundedObservabilityProducerV1, ObservabilityEmissionOutcomeV1, emit_index,
    emit_retrieval_pipeline,
};
use tracedecay_domain::{
    CoverageStateV1, IndexObservationKindV1, IndexObservedV1, IndexOutcomeV1, QueueDepthBucketV1,
    RetrievalBudget,
};
use tracedecay_query::retrieval::AuthorizedQueryFallbackV1;
use tracedecay_query::retrieval::observation::observe_composition;

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
