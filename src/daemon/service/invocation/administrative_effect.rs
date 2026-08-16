//! Administrative effect receipts minted synchronously by the daemon.
//!
//! Work commands, workflow runs, and handoff grants all mint the same
//! receipt shape; only the family token that namespaces every digest domain,
//! policy id, idempotency key, and effect id differs. One builder keeps the
//! shape identical across families instead of letting copies drift.

use serde::Serialize;
use tracedecay_application::{
    ApplicationContractError, ApplicationOutcome, AuthorityReceipt, Deadline, EffectId,
    EffectReceipt, EffectResult, EffectTermination, IdempotencyKey, OperationBudgetUsage,
    OperationReceipt, PolicyDecisionRef, ReconciliationState, RequestContext, RequestId,
};
use tracedecay_domain::{ComponentVersion, ManifestDigest, UtcMicros, canonical_sha256};
use tracedecay_tool_catalog::{EffectClass, UseCaseId};

use super::{RegisteredWorkRuntime, current_micros};

/// Policy-bound authority receipt and completed operation receipt for one
/// admitted operation of the `family` request family.
pub(super) fn administrative_authority(
    family: &'static str,
    registered: &RegisteredWorkRuntime,
    context: &RequestContext,
    operation_key: &str,
    use_case: &UseCaseId,
    observed_at: UtcMicros,
    deadline: Deadline,
) -> Result<(AuthorityReceipt, OperationReceipt), ApplicationContractError> {
    let policy_digest = canonical_sha256(&(
        format!("tracedecay.daemon.{family}-policy.v1"),
        &registered.policy_digest,
        &registered.grant.digest,
        operation_key,
        use_case,
    ))?;
    let policy = PolicyDecisionRef::new(
        format!("policy.daemon.{family}.{operation_key}.v1"),
        1,
        policy_digest,
        ComponentVersion::new(format!("tracedecay.daemon.{family}-policy.v1")).map_err(|_| {
            ApplicationContractError::Inconsistent {
                field: "administrative policy evaluator",
            }
        })?,
    )?;
    let authority = AuthorityReceipt::from_context(context, policy, observed_at)?;
    let execution = OperationReceipt::completed(
        observed_at,
        current_micros(),
        deadline,
        OperationBudgetUsage::default(),
    )?;
    Ok((authority, execution))
}

/// A completed administrative command effect: an `EffectId`, an idempotency
/// key, and a durable effect receipt asserting the committed state change,
/// with every digest domain namespaced under the `family` token.
#[allow(clippy::too_many_arguments)]
pub(super) fn administrative_command_effect<T>(
    family: &'static str,
    registered: &RegisteredWorkRuntime,
    context: &RequestContext,
    request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    result: T,
    observed_at: UtcMicros,
    deadline: Deadline,
) -> Result<ApplicationOutcome<T>, ApplicationContractError>
where
    T: Serialize,
{
    let (authority, execution) = administrative_authority(
        family,
        registered,
        context,
        operation_key,
        &use_case,
        observed_at,
        deadline,
    )?;
    let suffix = input_digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or(ApplicationContractError::Inconsistent {
            field: "administrative input digest",
        })?
        .to_owned();
    let idempotency_key = IdempotencyKey::new(format!("{family}.{operation_key}.{suffix}"))?;
    let expected_state = canonical_sha256(&(
        format!("tracedecay.{family}.expected-state.v1"),
        operation_key,
        &input_digest,
    ))?;
    let committed_state = canonical_sha256(&(
        format!("tracedecay.{family}.committed-state.v1"),
        operation_key,
        &result,
    ))?;
    let receipt = EffectReceipt {
        operation: use_case,
        request_id,
        actor: registered.actor.clone(),
        scope: context.scope().clone(),
        effect_class: EffectClass::Administrative,
        idempotency_key: idempotency_key.clone(),
        input_digest,
        expected_state: expected_state.clone(),
        policy_digest: authority.policy.digest.clone(),
        configuration_digest: registered.configuration_digest.clone(),
        catalog_digest: canonical_sha256(&(
            format!("tracedecay.{family}.catalog.v1"),
            operation_key,
        ))?,
        privacy_digest: canonical_sha256(&(
            format!("tracedecay.{family}.privacy.v1"),
            context.scope(),
            context.grant().disclosure,
        ))?,
        outcome: EffectTermination::Completed,
        committed_state: Some(committed_state),
        external_proof: None,
    };
    Ok(ApplicationOutcome::Effect(EffectResult::new(
        EffectId::new(format!("effect.{family}.{operation_key}.{suffix}"))?,
        EffectClass::Administrative,
        idempotency_key,
        authority,
        expected_state,
        execution,
        ReconciliationState::Reconciled,
        receipt,
        Some(result),
    )?))
}
