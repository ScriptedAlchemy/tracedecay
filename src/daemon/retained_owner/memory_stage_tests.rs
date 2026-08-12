use std::collections::BTreeSet;

use tracedecay_application::retained_surfaces::RetainedSurfaceOperation;
use tracedecay_application::{
    CancellationContext, CancellationSignal, CancellationStage, CapabilityGrantId,
    CapabilityGrantSnapshot, Deadline, DisclosureClass, RequestContext, RequestId,
    RetainedSurfaceExecutionContextV1, RetainedSurfaceExecutionErrorV1,
    retained_surface_application_operation,
};
use tracedecay_domain::{ActorId, ManifestDigest, ProjectId, RepositoryId, UtcMicros, WorktreeId};

use super::super::memory_mapping::{EFFECT_CANCELLATION_STAGES, READ_CANCELLATION_STAGES};
use super::bounded_memory_stage_for_test;

fn cancelled_execution(
    operation: RetainedSurfaceOperation,
) -> (
    RequestContext,
    CancellationSignal,
    tracedecay_application::ApplicationOperation,
) {
    let application_operation =
        retained_surface_application_operation(operation).expect("retained operation");
    let scope = tracedecay_application::ResolvedScope::new(
        ProjectId::new("project.memory-stage").expect("project"),
        RepositoryId::new("repository.memory-stage").expect("repository"),
        WorktreeId::new("worktree.memory-stage").expect("worktree"),
        None,
    )
    .expect("scope");
    let cancellation_id = "cancel.memory-stage";
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.memory-stage").expect("grant"),
        1,
        ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("digest"),
        ActorId::new("actor.memory-stage.issuer").expect("issuer"),
        UtcMicros(1),
        UtcMicros(i64::MAX - 1),
        scope.clone(),
        BTreeSet::from([application_operation.capability_id().clone()]),
        BTreeSet::from([application_operation.use_case_id().clone()]),
        DisclosureClass::Evidence,
    )
    .expect("grant");
    let context = RequestContext::new(
        ActorId::new("actor.memory-stage.caller").expect("actor"),
        scope,
        grant,
        RequestId::new("request.memory-stage").expect("request"),
        Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
        CancellationContext::active(cancellation_id).expect("cancellation context"),
    )
    .expect("request context");
    let signal = CancellationSignal::active(cancellation_id).expect("signal");
    assert!(signal.cancel(UtcMicros(2)));
    (context, signal, application_operation)
}

#[tokio::test]
async fn mutation_prerequisite_cancellation_is_before_read() {
    let (context, signal, operation) = cancelled_execution(RetainedSurfaceOperation::FactStoreAdd);
    let execution = RetainedSurfaceExecutionContextV1 {
        request_context: &context,
        cancellation_signal: &signal,
        operation: &operation,
        observed_at: UtcMicros(1),
    };
    let result = bounded_memory_stage_for_test(&execution, READ_CANCELLATION_STAGES).await;
    assert_eq!(
        result,
        Err(RetainedSurfaceExecutionErrorV1::Cancelled(
            CancellationStage::BeforeRead
        ))
    );
}

#[tokio::test]
async fn search_telemetry_cancellation_is_before_effect() {
    let (context, signal, operation) =
        cancelled_execution(RetainedSurfaceOperation::FactStoreSearch);
    let execution = RetainedSurfaceExecutionContextV1 {
        request_context: &context,
        cancellation_signal: &signal,
        operation: &operation,
        observed_at: UtcMicros(1),
    };
    let result = bounded_memory_stage_for_test(&execution, EFFECT_CANCELLATION_STAGES).await;
    assert_eq!(
        result,
        Err(RetainedSurfaceExecutionErrorV1::Cancelled(
            CancellationStage::BeforeEffect
        ))
    );
}
