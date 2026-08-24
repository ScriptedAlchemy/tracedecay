use std::sync::Arc;

use tracedecay_semantic::{
    SemanticEvaluationCancellationV1, SemanticExecutionAuthority, SemanticExecutionInterruptionV1,
    SemanticRuntimeScheduleCancellationV1, SemanticRuntimeScheduleFailureV1,
};

struct ExpiredAuthority;

impl SemanticExecutionAuthority for ExpiredAuthority {
    fn interruption(&self) -> Option<SemanticExecutionInterruptionV1> {
        Some(SemanticExecutionInterruptionV1::DeadlineExceeded)
    }
}

impl SemanticEvaluationCancellationV1 for ExpiredAuthority {}

#[test]
fn linked_schedule_preserves_typed_deadline_expiry() {
    let authority: Arc<dyn SemanticEvaluationCancellationV1> = Arc::new(ExpiredAuthority);
    let schedule = SemanticRuntimeScheduleCancellationV1::new_linked(1, authority);

    assert_eq!(
        schedule.failure(),
        Some(SemanticRuntimeScheduleFailureV1::DeadlineExceeded)
    );
}
