mod common;

use tracedecay_application::{
    EffectId, EffectReceipt, EffectResult, EffectTermination, IdempotencyKey, OperationReceipt,
    OperationTermination, ReconciliationState,
};
use tracedecay_domain::UtcMicros;
use tracedecay_tool_catalog::EffectClass;

#[test]
fn effect_unknown_stays_in_an_admitted_effect_receipt() {
    let operation = common::operation();
    let context = common::context(&operation);
    let execution = OperationReceipt {
        started_at: UtcMicros(2),
        ended_at: UtcMicros(3),
        effective_deadline: context.deadline().clone(),
        cancellation: None,
        budget: Default::default(),
        termination: OperationTermination::EffectUnknown,
    };
    let receipt = EffectReceipt {
        operation: operation.use_case_id().clone(),
        request_id: context.request_id().clone(),
        actor: context.actor().clone(),
        scope: context.scope().clone(),
        effect_class: EffectClass::SourceEdit,
        idempotency_key: IdempotencyKey::new("idempotency.fixture").unwrap(),
        input_digest: common::digest(common::SHA256_A),
        expected_state: common::digest(common::SHA256_A),
        policy_digest: common::digest(common::SHA256_B),
        configuration_digest: common::digest(common::SHA256_A),
        catalog_digest: common::digest(common::SHA256_B),
        privacy_digest: common::digest(common::SHA256_A),
        outcome: EffectTermination::EffectUnknown,
        committed_state: None,
        external_proof: None,
    };
    assert!(
        EffectResult::new(
            EffectId::new("effect.mismatched-state.fixture").unwrap(),
            EffectClass::SourceEdit,
            IdempotencyKey::new("idempotency.fixture").unwrap(),
            common::authority(&context),
            common::digest(common::SHA256_B),
            execution.clone(),
            ReconciliationState::Pending,
            receipt.clone(),
            None::<()>,
        )
        .is_err()
    );

    let effect = EffectResult::new(
        EffectId::new("effect.fixture").unwrap(),
        EffectClass::SourceEdit,
        IdempotencyKey::new("idempotency.fixture").unwrap(),
        common::authority(&context),
        common::digest(common::SHA256_A),
        execution,
        ReconciliationState::Pending,
        receipt,
        None::<()>,
    )
    .unwrap();

    assert_eq!(effect.receipt.outcome, EffectTermination::EffectUnknown);
    assert_eq!(effect.reconciliation, ReconciliationState::Pending);
    assert!(effect.payload.is_none());
}
