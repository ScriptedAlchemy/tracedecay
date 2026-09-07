use super::*;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use tracedecay_application::retrieval::MAX_APPLICATION_PAGE_SIZE;

struct CancelDuringTraversal {
    checks: AtomicUsize,
}

impl GraphExecutionControl for CancelDuringTraversal {
    fn is_cancelled(&self) -> bool {
        self.checks.fetch_add(1, Ordering::SeqCst) >= 2
    }

    fn elapsed_micros(&self) -> u64 {
        0
    }
}

struct CancelAfterChecks {
    checks: AtomicUsize,
    cancel_at: usize,
}

impl GraphExecutionControl for CancelAfterChecks {
    fn is_cancelled(&self) -> bool {
        self.checks.fetch_add(1, Ordering::SeqCst) >= self.cancel_at
    }

    fn elapsed_micros(&self) -> u64 {
        0
    }
}

struct DeadlineDuringTraversal {
    elapsed: AtomicU64,
}

impl GraphExecutionControl for DeadlineDuringTraversal {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn elapsed_micros(&self) -> u64 {
        self.elapsed.fetch_add(1, Ordering::SeqCst)
    }
}

struct CancelLifecycleDuringTraversal {
    checks: AtomicUsize,
    lifecycle: CancellationSignal,
}

impl GraphExecutionControl for CancelLifecycleDuringTraversal {
    fn is_cancelled(&self) -> bool {
        if self.checks.fetch_add(1, Ordering::SeqCst) >= 2 {
            self.lifecycle.cancel(UtcMicros(1));
        }
        false
    }

    fn elapsed_micros(&self) -> u64 {
        0
    }
}

fn projection_reader(request: &GraphLaneRequest) -> CodeGraphEvidenceReader {
    projection_reader_with_lifecycle(request).0
}

fn projection_reader_with_lifecycle(
    request: &GraphLaneRequest,
) -> (CodeGraphEvidenceReader, CancellationSignal) {
    let cancellation =
        CancellationSignal::active("cancellation.code-graph.control").expect("valid token");
    let publisher =
        HermeticCodeGraphProjectionStore::memory(&cancellation).expect("open memory graph");
    publish_projection(
        &publisher,
        request,
        &[CanonicalRelationEdgeV1 {
            from_occurrence: id("symbol.seed"),
            to_occurrence: id("symbol.target"),
            kind: RelationEdgeKindV1::Calls,
            authority: EdgeAuthorityV1::SyntaxExact,
            evidence_span: SourceSpan {
                start_byte: 0,
                end_byte: 1,
            },
        }],
        &["symbol.seed", "symbol.target"],
    );
    let reader = publisher
        .verified_store(&request.generation)
        .expect("open verified generation")
        .evidence_reader(
            &request.generation,
            None,
            freshness(FreshnessCompatibilityV1::Current),
            &cancellation,
        )
        .expect("published generation is readable");
    (reader, cancellation)
}

#[test]
fn graph_traversal_observes_request_cancellation() {
    let request = graph_request(8, 2);
    let reader = projection_reader(&request);

    let result = reader.read_graph_evidence(
        &request,
        Arc::new(TestGraphExecutionControl {
            cancelled: true,
            elapsed_micros: 0,
        }),
    );

    assert_eq!(result, Err(RetrievalPortError::Cancelled));
}

#[test]
fn graph_traversal_observes_request_deadline() {
    let mut request = graph_request(8, 2);
    request.budget.deadline_micros = Some(5);
    let reader = projection_reader(&request);

    let result = reader.read_graph_evidence(
        &request,
        Arc::new(TestGraphExecutionControl {
            cancelled: false,
            elapsed_micros: 5,
        }),
    );

    assert_eq!(result, Err(RetrievalPortError::BudgetExceeded));
}

#[test]
fn graph_traversal_rechecks_cancellation_during_native_walk() {
    let request = graph_request(8, 2);
    let reader = projection_reader(&request);

    let result = reader.read_graph_evidence(
        &request,
        Arc::new(CancelDuringTraversal {
            checks: AtomicUsize::new(0),
        }),
    );

    assert_eq!(result, Err(RetrievalPortError::Cancelled));
}

#[test]
fn graph_traversal_rechecks_deadline_during_native_walk() {
    let mut request = graph_request(8, 2);
    request.budget.deadline_micros = Some(2);
    let reader = projection_reader(&request);

    let result = reader.read_graph_evidence(
        &request,
        Arc::new(DeadlineDuringTraversal {
            elapsed: AtomicU64::new(0),
        }),
    );

    assert_eq!(result, Err(RetrievalPortError::BudgetExceeded));
}

#[test]
fn graph_traversal_rechecks_reader_lifecycle_during_native_walk() {
    let request = graph_request(8, 2);
    let (reader, lifecycle) = projection_reader_with_lifecycle(&request);

    let result = reader.read_graph_evidence(
        &request,
        Arc::new(CancelLifecycleDuringTraversal {
            checks: AtomicUsize::new(0),
            lifecycle,
        }),
    );

    assert_eq!(result, Err(RetrievalPortError::Cancelled));
}

fn large_graph_batch(request: &GraphLaneRequest) -> RetrieverBatch<GraphLaneEvidence> {
    let pairs = (0..128)
        .map(|index| {
            graph_pair(
                request,
                &format!("occ.target.{index:03}"),
                &["symbol.seed", &format!("symbol.target.{index:03}")],
                &[EdgeAuthorityV1::SyntaxExact],
                1_000_000 - index,
            )
        })
        .collect::<Vec<_>>();
    batch(pairs, candidate_coverage(128))
}

#[test]
fn graph_postprocessing_observes_cancellation_before_cloning_large_result() {
    let request = graph_request(8, 2);
    let lane = GraphLane::new(FakeGraphPort::outcome(RetrieverOutcome::Complete(
        large_graph_batch(&request),
    )));

    assert_eq!(
        lane.retrieve_graph(
            &request,
            Arc::new(CancelDuringTraversal {
                checks: AtomicUsize::new(0),
            }),
        ),
        Err(RetrievalPortError::Cancelled)
    );
}

#[test]
fn graph_postprocessing_observes_deadline_before_cloning_large_result() {
    let mut request = graph_request(8, 2);
    request.budget.deadline_micros = Some(2);
    let lane = GraphLane::new(FakeGraphPort::outcome(RetrieverOutcome::Complete(
        large_graph_batch(&request),
    )));

    assert_eq!(
        lane.retrieve_graph(
            &request,
            Arc::new(DeadlineDuringTraversal {
                elapsed: AtomicU64::new(0),
            }),
        ),
        Err(RetrievalPortError::BudgetExceeded)
    );
}

#[test]
fn graph_postprocessing_rechecks_control_after_bounded_sort() {
    let request = graph_request(128, 2);
    let lane = GraphLane::new(FakeGraphPort::outcome(RetrieverOutcome::Complete(
        large_graph_batch(&request),
    )));

    assert_eq!(
        lane.retrieve_graph(
            &request,
            Arc::new(CancelAfterChecks {
                checks: AtomicUsize::new(0),
                cancel_at: 129,
            }),
        ),
        Err(RetrievalPortError::Cancelled)
    );
}

#[test]
fn graph_request_rejects_unbounded_candidate_materialization() {
    let mut request = graph_request(8, 2);
    request.budget.max_candidates_per_lane = MAX_APPLICATION_PAGE_SIZE + 1;

    assert!(matches!(
        request.validate(),
        Err(RetrievalPortError::Contract(_))
    ));
}
