use serde_json::{Value, json};
use tracedecay_domain::{ActorId, ManifestDigest, canonical_sha256};
use tracedecay_tool_catalog::EffectClass;

use super::{
    AutomationRunProblemV1, AutomationRunResultV1, automatic_fact_terminal, automation_request,
    memory_scope, zero_terminal,
};
use crate::retained_surfaces::{
    RetainedSurfaceExecutionErrorV1, RetainedSurfaceOperation,
    retained_surface_application_operation, retained_surface_execution_problem,
};
use crate::{
    ApplicationProblemEnvelope, EffectReceipt, EffectTermination, IdempotencyKey, RequestId,
};

fn outer_delivery_partial(result: AutomationRunResultV1) -> Value {
    let request_id = RequestId::new("request.automation.outer-partial").expect("request");
    let scope = memory_scope();
    let operation =
        retained_surface_application_operation(RetainedSurfaceOperation::FactStoreCurate)
            .expect("operation");
    let committed_state = canonical_sha256(&(
        "tracedecay.retained.effect.committed-state.v1",
        RetainedSurfaceOperation::FactStoreCurate.as_str(),
        result.run_id.as_str(),
        &result,
    ))
    .expect("committed state");
    let digest = |seed: char| {
        ManifestDigest::new(format!("sha256:{}", seed.to_string().repeat(64))).expect("digest")
    };
    let problem =
        retained_surface_execution_problem(RetainedSurfaceExecutionErrorV1::PartialEffect {
            reason_code: "application.retained.effect-delivery-failed".to_owned(),
            committed_receipt: Box::new(EffectReceipt {
                operation: operation.use_case_id().clone(),
                request_id: request_id.clone(),
                actor: ActorId::new("actor.automation").expect("actor"),
                scope: scope.clone(),
                effect_class: EffectClass::Administrative,
                idempotency_key: IdempotencyKey::new("idempotency.outer-partial").expect("key"),
                input_digest: digest('1'),
                expected_state: digest('2'),
                policy_digest: digest('3'),
                configuration_digest: digest('4'),
                catalog_digest: digest('5'),
                privacy_digest: digest('6'),
                outcome: EffectTermination::Partial,
                committed_state: Some(committed_state),
                external_proof: None,
            }),
            detail: "The outer result committed before delivery expired".to_owned(),
        });
    let envelope = ApplicationProblemEnvelope::new(
        operation.result_contract().clone(),
        request_id.clone(),
        problem,
    )
    .expect("envelope");
    let terminal = AutomationRunProblemV1::new_outer_effect_partial(
        &automation_request(result.run_id.as_str(), result.task),
        scope,
        envelope,
        result,
        &request_id,
    )
    .expect("outer partial");
    serde_json::to_value(terminal).expect("wire")
}

fn assert_bound_outer_partial(result: AutomationRunResultV1) {
    let wire = outer_delivery_partial(result);
    assert!(serde_json::from_value::<AutomationRunProblemV1>(wire.clone()).is_ok());
    let original_reviewed = wire["committed_outer_result"]["terminal"]["summary"]["reviewed_count"]
        .as_u64()
        .expect("reviewed count");
    let mut changed = wire;
    changed["committed_outer_result"]["terminal"]["summary"]["reviewed_count"] =
        json!(original_reviewed + 1);
    assert!(serde_json::from_value::<AutomationRunProblemV1>(changed).is_err());
}

#[test]
fn zero_inner_effect_outer_delivery_failure_is_a_bound_partial_terminal() {
    let result = serde_json::from_value(zero_terminal("completed")).expect("zero-effect result");
    assert_bound_outer_partial(result);
}

#[test]
fn nonempty_inner_effect_outer_delivery_failure_is_a_bound_partial_terminal() {
    let result = serde_json::from_value(automatic_fact_terminal()).expect("nonempty result");
    assert_bound_outer_partial(result);
}
