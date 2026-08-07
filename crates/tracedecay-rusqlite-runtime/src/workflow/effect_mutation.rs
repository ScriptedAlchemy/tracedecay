//! Transactional workflow mutations applied from durable preparations.

use tracedecay_application::{
    TaskHandoffGrant, TaskHandoffRedeemed, TaskHandoffScope, WorkflowDefinitionDisposition,
    WorkflowDefinitionLifecycleCommand, WorkflowDefinitionTransitionOutcome,
    WorkflowEffectAuthorityErrorV1, WorkflowEffectMutationV1, WorkflowEffectOutcomeV1,
    WorkflowEffectPreparedV1, WorkflowEffectProblemV1, WorkflowEffectSuccessV1,
    WorkflowLifecycleOperation,
};
use tracedecay_domain::{ManifestDigest, UtcMicros, WorkflowDefinition};

use crate::exact_sql::{ExactSqlTransaction, ExactSqlValue};

use super::{
    decode_json, definition_digest, encode_definition, encode_json, execute_tx, execute_tx_changed,
    query_tx, sql_integer, sql_text, version_i64, workflow_effect_codec_unavailable,
    workflow_effect_unavailable,
};

pub(super) fn apply_workflow_effect(
    transaction: &ExactSqlTransaction,
    prepared: &WorkflowEffectPreparedV1,
    applied_at: UtcMicros,
) -> Result<WorkflowEffectOutcomeV1, WorkflowEffectAuthorityErrorV1> {
    match prepared.mutation() {
        WorkflowEffectMutationV1::RegisterDefinition(definition) => {
            apply_definition_registration(transaction, definition, applied_at)
        }
        WorkflowEffectMutationV1::ActivateDefinition(command)
        | WorkflowEffectMutationV1::RetireDefinition(command)
        | WorkflowEffectMutationV1::RejectDefinition(command) => {
            apply_lifecycle_command(transaction, command)
        }
        WorkflowEffectMutationV1::HandoffIssue(grant) => apply_handoff_issue(transaction, grant),
        WorkflowEffectMutationV1::HandoffRedeem {
            token_digest,
            expected_scope,
            consumed_at,
        } => apply_handoff_redeem(transaction, token_digest, expected_scope, *consumed_at),
        WorkflowEffectMutationV1::Problem(problem) => {
            Ok(WorkflowEffectOutcomeV1::Problem(*problem))
        }
    }
}

/// Applies one compare-and-swap lifecycle transition and maps its typed
/// outcome onto the durable effect contract.
///
/// Plan 32 keeps retire and reject terminal, so an illegal edge and a stale
/// expected revision are both reported as conflicts rather than silently
/// coerced; a replayed command returns the stored disposition unchanged.
fn apply_lifecycle_command(
    transaction: &ExactSqlTransaction,
    command: &WorkflowDefinitionLifecycleCommand,
) -> Result<WorkflowEffectOutcomeV1, WorkflowEffectAuthorityErrorV1> {
    let outcome = super::disposition::apply_lifecycle_transition(transaction, command)
        .map_err(|_| workflow_effect_codec_unavailable())?;
    Ok(match outcome {
        WorkflowDefinitionTransitionOutcome::Applied(disposition)
        | WorkflowDefinitionTransitionOutcome::Replayed(disposition) => {
            WorkflowEffectOutcomeV1::Success(lifecycle_success(command.operation, disposition))
        }
        WorkflowDefinitionTransitionOutcome::RevisionConflict(_)
        | WorkflowDefinitionTransitionOutcome::IllegalTransition(_) => {
            WorkflowEffectOutcomeV1::Problem(WorkflowEffectProblemV1::Conflict)
        }
        WorkflowDefinitionTransitionOutcome::Missing => {
            WorkflowEffectOutcomeV1::Problem(WorkflowEffectProblemV1::NotFoundOrNotAuthorized)
        }
    })
}

fn lifecycle_success(
    operation: WorkflowLifecycleOperation,
    disposition: WorkflowDefinitionDisposition,
) -> WorkflowEffectSuccessV1 {
    match operation {
        WorkflowLifecycleOperation::Activate => {
            WorkflowEffectSuccessV1::DefinitionActivated(Box::new(disposition))
        }
        WorkflowLifecycleOperation::Retire => {
            WorkflowEffectSuccessV1::DefinitionRetired(Box::new(disposition))
        }
        WorkflowLifecycleOperation::Reject => {
            WorkflowEffectSuccessV1::DefinitionRejected(Box::new(disposition))
        }
    }
}

fn apply_definition_registration(
    transaction: &ExactSqlTransaction,
    definition: &WorkflowDefinition,
    registered_at: UtcMicros,
) -> Result<WorkflowEffectOutcomeV1, WorkflowEffectAuthorityErrorV1> {
    let version = version_i64(definition.definition_version())
        .map_err(|_| workflow_effect_codec_unavailable())?;
    let payload = encode_definition(definition).map_err(|_| workflow_effect_codec_unavailable())?;
    let digest = definition_digest(definition).map_err(|_| workflow_effect_codec_unavailable())?;
    let existing = query_tx(
        transaction,
        "SELECT payload_digest FROM workflow_definition_source_journal
         WHERE definition_id = ?1 AND definition_version = ?2",
        vec![
            ExactSqlValue::Text(definition.definition_id().as_str().to_owned()),
            ExactSqlValue::Integer(version),
        ],
    )
    .map_err(workflow_effect_unavailable)?;
    if let Some(row) = existing.rows.first() {
        let existing_digest =
            sql_text(&row.values, 0).ok_or_else(workflow_effect_codec_unavailable)?;
        if existing_digest != digest.as_str() {
            return Ok(WorkflowEffectOutcomeV1::Problem(
                WorkflowEffectProblemV1::InvalidRequest,
            ));
        }
        super::disposition::seed_candidate_disposition(
            transaction,
            definition.definition_id(),
            definition.definition_version(),
            registered_at,
        )
        .map_err(|_| workflow_effect_codec_unavailable())?;
        return Ok(WorkflowEffectOutcomeV1::Success(
            WorkflowEffectSuccessV1::DefinitionRegistered(Box::new(definition.clone())),
        ));
    }
    execute_tx(
        transaction,
        "INSERT INTO workflow_definition_source_journal (
             definition_id, definition_version, payload, payload_digest
         ) VALUES (?1, ?2, ?3, ?4)",
        vec![
            ExactSqlValue::Text(definition.definition_id().as_str().to_owned()),
            ExactSqlValue::Integer(version),
            ExactSqlValue::Text(payload),
            ExactSqlValue::Text(digest.as_str().to_owned()),
        ],
    )
    .map_err(workflow_effect_unavailable)?;
    super::disposition::seed_candidate_disposition(
        transaction,
        definition.definition_id(),
        definition.definition_version(),
        registered_at,
    )
    .map_err(|_| workflow_effect_codec_unavailable())?;
    Ok(WorkflowEffectOutcomeV1::Success(
        WorkflowEffectSuccessV1::DefinitionRegistered(Box::new(definition.clone())),
    ))
}

fn apply_handoff_issue(
    transaction: &ExactSqlTransaction,
    grant: &TaskHandoffGrant,
) -> Result<WorkflowEffectOutcomeV1, WorkflowEffectAuthorityErrorV1> {
    let existing = query_tx(
        transaction,
        "SELECT 1 FROM workflow_handoffs WHERE token_digest = ?1",
        vec![ExactSqlValue::Text(
            grant.token_digest().as_str().to_owned(),
        )],
    )
    .map_err(workflow_effect_unavailable)?;
    if !existing.rows.is_empty() {
        return Ok(WorkflowEffectOutcomeV1::Problem(
            WorkflowEffectProblemV1::InvalidRequest,
        ));
    }
    let scope_payload =
        encode_json(grant.scope()).map_err(|_| workflow_effect_codec_unavailable())?;
    execute_tx(
        transaction,
        "INSERT INTO workflow_handoffs (
             token_digest, scope_payload, issued_at, expires_at, consumed
         ) VALUES (?1, ?2, ?3, ?4, 0)",
        vec![
            ExactSqlValue::Text(grant.token_digest().as_str().to_owned()),
            ExactSqlValue::Text(scope_payload),
            ExactSqlValue::Integer(grant.issued_at().0),
            ExactSqlValue::Integer(grant.expires_at().0),
        ],
    )
    .map_err(workflow_effect_unavailable)?;
    Ok(WorkflowEffectOutcomeV1::Success(
        WorkflowEffectSuccessV1::HandoffIssued(Box::new(grant.clone())),
    ))
}

fn apply_handoff_redeem(
    transaction: &ExactSqlTransaction,
    token_digest: &ManifestDigest,
    expected_scope: &TaskHandoffScope,
    consumed_at: UtcMicros,
) -> Result<WorkflowEffectOutcomeV1, WorkflowEffectAuthorityErrorV1> {
    let rows = query_tx(
        transaction,
        "SELECT scope_payload, expires_at, consumed FROM workflow_handoffs
         WHERE token_digest = ?1",
        vec![ExactSqlValue::Text(token_digest.as_str().to_owned())],
    )
    .map_err(workflow_effect_unavailable)?;
    let Some(row) = rows.rows.first() else {
        return Ok(WorkflowEffectOutcomeV1::Problem(
            WorkflowEffectProblemV1::NotFoundOrNotAuthorized,
        ));
    };
    let scope_payload = sql_text(&row.values, 0).ok_or_else(workflow_effect_codec_unavailable)?;
    let scope: TaskHandoffScope =
        decode_json(scope_payload).map_err(|_| workflow_effect_codec_unavailable())?;
    if &scope != expected_scope {
        return Ok(WorkflowEffectOutcomeV1::Problem(
            WorkflowEffectProblemV1::NotFoundOrNotAuthorized,
        ));
    }
    let expires_at = sql_integer(&row.values, 1).ok_or_else(workflow_effect_codec_unavailable)?;
    let consumed = sql_integer(&row.values, 2).ok_or_else(workflow_effect_codec_unavailable)?;
    if consumed_at.0 >= expires_at || consumed != 0 {
        return Ok(WorkflowEffectOutcomeV1::Problem(
            WorkflowEffectProblemV1::InvalidRequest,
        ));
    }
    let changed = execute_tx_changed(
        transaction,
        "UPDATE workflow_handoffs SET consumed = 1
         WHERE token_digest = ?1 AND consumed = 0",
        vec![ExactSqlValue::Text(token_digest.as_str().to_owned())],
    )
    .map_err(workflow_effect_unavailable)?;
    if changed != 1 {
        return Err(WorkflowEffectAuthorityErrorV1::InvalidTransition);
    }
    Ok(WorkflowEffectOutcomeV1::Success(
        WorkflowEffectSuccessV1::HandoffRedeemed(Box::new(TaskHandoffRedeemed {
            scope: expected_scope.clone(),
        })),
    ))
}
