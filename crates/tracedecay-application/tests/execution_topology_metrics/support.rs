use std::sync::atomic::{AtomicUsize, Ordering};

use tracedecay_application::{
    ExecutionTopologyRollupFragmentPageV1, ExecutionTopologyRollupFragmentQueryV1,
    ExecutionTopologyRollupQueryPort, ObservabilityFuture, ObservabilityPageV1,
    ObservabilityQueryPort, ObservabilityQueryV1,
};
use tracedecay_domain::CoverageStateV1;

pub(super) struct CountingObservations {
    queries: AtomicUsize,
}

impl CountingObservations {
    pub(super) const fn new() -> Self {
        Self {
            queries: AtomicUsize::new(0),
        }
    }

    pub(super) fn query_count(&self) -> usize {
        self.queries.load(Ordering::SeqCst)
    }
}

impl ObservabilityQueryPort for CountingObservations {
    fn query<'a>(
        &'a self,
        _query: ObservabilityQueryV1,
    ) -> ObservabilityFuture<'a, ObservabilityPageV1> {
        self.queries.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Ok(ObservabilityPageV1 {
                events: Vec::new(),
                event_cursors: Vec::new(),
                watermark: "counting-observations".to_owned(),
                coverage: CoverageStateV1::Known,
                next_watermark: None,
            })
        })
    }
}

pub(super) struct NeverRollupPort;

impl ExecutionTopologyRollupQueryPort for NeverRollupPort {
    fn query_rollup_fragments<'a>(
        &'a self,
        _query: ExecutionTopologyRollupFragmentQueryV1,
    ) -> ObservabilityFuture<'a, ExecutionTopologyRollupFragmentPageV1> {
        panic!("a one-partial-day topology metrics read must not query retained rollups")
    }
}
