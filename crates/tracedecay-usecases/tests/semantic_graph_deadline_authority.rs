use std::sync::Arc;
use std::time::{Duration, Instant};

use tracedecay_graph_db::{GraphCancellation, GraphDbError, NeverCancelled};
use tracedecay_usecases::semantic_runtime::SemanticGraphExecutionAuthorityV1;

struct Cancelled;

impl GraphCancellation for Cancelled {
    fn is_cancelled(&self) -> bool {
        true
    }
}

#[test]
fn expired_graph_authority_surfaces_typed_deadline() {
    let authority =
        SemanticGraphExecutionAuthorityV1::new(Arc::new(NeverCancelled), Instant::now());

    assert_eq!(authority.checkpoint(), Err(GraphDbError::DeadlineExceeded));
}

#[test]
fn graph_authority_preserves_cancellation_before_deadline_expiry() {
    let authority = SemanticGraphExecutionAuthorityV1::new(
        Arc::new(Cancelled),
        Instant::now() + Duration::from_secs(30),
    );

    assert_eq!(authority.checkpoint(), Err(GraphDbError::Cancelled));
}
