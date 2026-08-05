use tracedecay_application::{
    ApplicationOperation, CancellationObservation, CancellationStage, EffectTermination,
    ReconciliationState, SourceEditAuthorizationPort, SourceEditRollbackRequestV1, now_micros,
    source_edit_rollback_operation,
};
use tracedecay_domain::ManifestDigest;

use crate::tracedecay::TraceDecay;
use tracedecay_runtime_core::errors::Result;

use super::JOURNAL_VERSION;
use super::control::SourceEditEffectControlV1;
use super::digest::{
    effect_id, normalize_candidate_files, source_edit_recovery_digest, source_edit_state_digest,
};
use super::journal::{
    SourceEditDurability, SourceEditDurableRequestV1, SourceEditJournalStateV1,
    SourceEditJournalV1, same_source_edit_authority,
};
use super::outcome::{SourceEditApplicationResult, SourceEditDurableOutcomeV1, SourceEditOutcome};
use super::reconcile::recover_source_edit_transaction;
use super::records::{applied_record, durable_record, interrupted_record, unknown_record};
use super::verify::{application_contract_error, application_problem, config_error};

fn durable_request(
    operation: &ApplicationOperation,
    request: &SourceEditRollbackRequestV1,
    authority: &tracedecay_application::SourceEditAuthorizationAdmissionV1,
) -> SourceEditDurableRequestV1 {
    SourceEditDurableRequestV1 {
        operation: operation.use_case_id().clone(),
        request_id: request.context.request_id().clone(),
        actor: request.context.actor().clone(),
        scope: request.context.scope().clone(),
        authority: authority.receipt.clone(),
        authority_proof: authority.proof.clone(),
        idempotency_key: request.idempotency_key.clone(),
        deadline: request.context.deadline().clone(),
        started_at: request.observed_at,
        dry_run: false,
        verification_requested: false,
    }
}

fn rollback_journal(
    operation: &ApplicationOperation,
    request: &SourceEditRollbackRequestV1,
    authority: &tracedecay_application::SourceEditAuthorizationAdmissionV1,
    input_digest: &ManifestDigest,
    predicted_state: Option<ManifestDigest>,
    candidate_files: Vec<String>,
    recovery_files: Vec<crate::tracedecay::PlannedSourceEditFile>,
) -> Result<SourceEditJournalV1> {
    let recovery_digest = (!recovery_files.is_empty())
        .then(|| source_edit_recovery_digest(&recovery_files))
        .transpose()?;
    Ok(SourceEditJournalV1 {
        version: JOURNAL_VERSION,
        effect_id: effect_id(&request.idempotency_key, input_digest)?,
        input_digest: input_digest.clone(),
        expected_state: request.expected_state.clone(),
        predicted_state,
        candidate_files,
        recovery_files,
        recovery_digest,
        request: durable_request(operation, request, authority),
        state: SourceEditJournalStateV1::Prepared,
    })
}

fn control_outcome(
    termination: EffectTermination,
    cancelled: &str,
    timed_out: &str,
) -> SourceEditOutcome {
    match termination {
        EffectTermination::Cancelled => SourceEditOutcome::Cancelled {
            message: cancelled.to_owned(),
        },
        EffectTermination::TimedOut => SourceEditOutcome::TimedOut {
            message: timed_out.to_owned(),
        },
        _ => unreachable!("source edit control only yields cancellation or timeout"),
    }
}

#[allow(clippy::too_many_arguments)]
fn persist_pre_effect(
    durability: &SourceEditDurability,
    operation: &ApplicationOperation,
    request: &SourceEditRollbackRequestV1,
    authority: &tracedecay_application::SourceEditAuthorizationAdmissionV1,
    input_digest: &ManifestDigest,
    outcome: SourceEditOutcome,
    termination: EffectTermination,
    observation: Option<CancellationObservation>,
) -> Result<SourceEditApplicationResult> {
    if let Some(stored) = durability.load_receipt(&request.idempotency_key)? {
        if stored.input_digest != *input_digest {
            return Err(config_error(
                "source edit rollback idempotency key conflicts with a prior input",
            ));
        }
        if stored.authority_proof != authority.proof {
            return Err(config_error(
                "source edit rollback receipt authority changed",
            ));
        }
        return Ok(stored.into_application_result(true));
    }
    let journal = rollback_journal(
        operation,
        request,
        authority,
        input_digest,
        None,
        Vec::new(),
        Vec::new(),
    )?;
    let record = durable_record(
        &journal,
        SourceEditDurableOutcomeV1::from_live(operation.use_case_id(), &outcome),
        None,
        now_micros(),
        termination,
        ReconciliationState::Reconciled,
        observation,
    )?;
    durability.persist_receipt(&record)?;
    Ok(record.into_live_application_result(outcome, None))
}

pub(super) async fn execute_source_edit_rollback_inner<A>(
    graph: &TraceDecay,
    operation: &ApplicationOperation,
    request: SourceEditRollbackRequestV1,
    authorization: &A,
    control: Option<&SourceEditEffectControlV1>,
) -> Result<SourceEditApplicationResult>
where
    A: SourceEditAuthorizationPort,
{
    request.validate().map_err(application_contract_error)?;
    let expected = source_edit_rollback_operation().map_err(application_contract_error)?;
    if operation != &expected {
        return Err(config_error(
            "source edit rollback request does not match its catalog operation",
        ));
    }
    let durability = SourceEditDurability::for_graph(graph);
    let _lock = durability.lock()?;
    let input_digest = request.input_digest().map_err(application_contract_error)?;
    let requested_authority = tracedecay_application::SourceEditAuthorizationAdmissionV1::new(
        request.authority.clone(),
        request.proof.clone(),
        request.context.scope(),
    )
    .map_err(application_contract_error)?;
    if let Some(stop) =
        control.and_then(|control| control.checkpoint(CancellationStage::BeforeAdmission))
    {
        let outcome = control_outcome(
            stop.termination,
            "source edit rollback was cancelled before admission",
            "source edit rollback timed out before admission",
        );
        return persist_pre_effect(
            &durability,
            operation,
            &request,
            &requested_authority,
            &input_digest,
            outcome,
            stop.termination,
            Some(stop.observation),
        );
    }
    let admission = match authorization
        .admit(&request.context, operation, request.observed_at)
        .await
    {
        Ok(admission)
            if admission.receipt == request.authority && admission.proof == request.proof =>
        {
            admission
        }
        Ok(_) | Err(_) => {
            return persist_pre_effect(
                &durability,
                operation,
                &request,
                &requested_authority,
                &input_digest,
                SourceEditOutcome::Failed {
                    message: "source edit rollback failed before the effect".to_owned(),
                },
                EffectTermination::Failed,
                None,
            );
        }
    };
    let current_authority = authorization
        .recheck_effect(&request.context, operation, &admission, now_micros())
        .await
        .map_err(application_problem)?;
    if !same_source_edit_authority(&current_authority.receipt, &request.authority)
        || current_authority.proof != request.proof
    {
        return persist_pre_effect(
            &durability,
            operation,
            &request,
            &admission,
            &input_digest,
            SourceEditOutcome::Failed {
                message: "source edit rollback failed before the effect".to_owned(),
            },
            EffectTermination::Failed,
            None,
        );
    }

    recover_source_edit_transaction(&durability, graph, request.context.scope()).await?;
    if let Some(stored) = durability.load_receipt(&request.idempotency_key)? {
        if stored.input_digest != input_digest {
            return Err(config_error(
                "source edit rollback idempotency key conflicts with a prior input",
            ));
        }
        if stored.authority_proof != request.proof
            || !same_source_edit_authority(&stored.effect.authority, &request.authority)
        {
            return Err(config_error(
                "source edit rollback replay authority changed",
            ));
        }
        return Ok(stored.into_application_result(true));
    }

    let retained = durability
        .load_rollback_record(&request.effect_id)?
        .ok_or_else(|| config_error("source edit effect has no retained rollback material"))?;
    let original = durability
        .load_receipt(&request.original_idempotency_key)?
        .ok_or_else(|| {
            config_error("source edit rollback is missing its original effect receipt")
        })?;
    if retained.effect_id != request.effect_id
        || retained.input_digest != request.original_input_digest
        || retained.idempotency_key != request.original_idempotency_key
        || retained.actor != *request.context.actor()
        || retained.scope != *request.context.scope()
        || retained.committed_state != request.expected_state
        || original.effect.effect_id != request.effect_id
        || original.input_digest != request.original_input_digest
        || original.effect.receipt.outcome != EffectTermination::Completed
        || original.effect.receipt.committed_state.as_ref() != Some(&retained.committed_state)
    {
        return Err(config_error(
            "source edit rollback identity does not match the completed original effect",
        ));
    }
    let candidate_files = normalize_candidate_files(
        graph.project_root(),
        retained
            .recovery_files
            .iter()
            .map(|file| file.relative_path.clone())
            .collect(),
    )?;
    let observed_state = source_edit_state_digest(graph.project_root(), &candidate_files)?;
    if observed_state != retained.committed_state {
        return persist_pre_effect(
            &durability,
            operation,
            &request,
            &current_authority,
            &input_digest,
            SourceEditOutcome::Failed {
                message: "source edit rollback refused stale or foreign workspace bytes".to_owned(),
            },
            EffectTermination::Failed,
            None,
        );
    }
    let recovery_files = retained
        .recovery_files
        .iter()
        .map(|file| crate::tracedecay::PlannedSourceEditFile {
            relative_path: file.relative_path.clone(),
            expected: file.intended.clone(),
            intended: file.expected.clone(),
        })
        .collect::<Vec<_>>();
    let mut journal = rollback_journal(
        operation,
        &request,
        &current_authority,
        &input_digest,
        Some(retained.expected_state.clone()),
        candidate_files,
        recovery_files,
    )?;
    durability.persist_journal(&journal)?;
    if let Some(stop) =
        control.and_then(|control| control.checkpoint(CancellationStage::BeforeEffect))
    {
        let outcome = control_outcome(
            stop.termination,
            "source edit rollback was cancelled before the effect",
            "source edit rollback timed out before the effect",
        );
        let record = interrupted_record(&journal, &outcome, stop)?;
        durability.persist_receipt(&record)?;
        durability.clear_journal()?;
        return Ok(record.into_live_application_result(outcome, None));
    }

    let apply_result = graph
        .apply_source_edit_rollback(&retained.recovery_files)
        .await;
    let committed_state = source_edit_state_digest(graph.project_root(), &journal.candidate_files)?;
    if apply_result.is_err() || committed_state != retained.expected_state {
        if committed_state != journal.expected_state {
            let outcome = SourceEditOutcome::EffectUnknown {
                message: "source edit rollback effect is unknown and requires reconciliation"
                    .to_owned(),
            };
            let record = unknown_record(&journal)?;
            durability.persist_receipt(&record)?;
            return Ok(record.into_live_application_result(outcome, None));
        }
        let outcome = SourceEditOutcome::Failed {
            message: "source edit rollback failed before changing workspace state".to_owned(),
        };
        let record = durable_record(
            &journal,
            SourceEditDurableOutcomeV1::from_live(operation.use_case_id(), &outcome),
            None,
            now_micros(),
            EffectTermination::Failed,
            ReconciliationState::Reconciled,
            None,
        )?;
        durability.persist_receipt(&record)?;
        durability.clear_journal()?;
        return Ok(record.into_live_application_result(outcome, None));
    }

    let outcome = SourceEditOutcome::Reconciled {
        success: true,
        message: "source edit rollback restored every retained preimage".to_owned(),
    };
    let ended_at = now_micros();
    let mut control_observation = control
        .and_then(|control| control.checkpoint(CancellationStage::EffectInFlight))
        .map(|stop| stop.observation);
    journal.state = SourceEditJournalStateV1::Applied {
        outcome: SourceEditDurableOutcomeV1::from_live(operation.use_case_id(), &outcome),
        committed_state: committed_state.clone(),
        ended_at,
        control_observation: control_observation.clone(),
        verification_state: None,
    };
    durability.persist_journal(&journal)?;
    if control_observation.is_none() {
        control_observation = control
            .and_then(|control| control.checkpoint(CancellationStage::AfterCommit))
            .map(|stop| stop.observation);
        if let SourceEditJournalStateV1::Applied {
            control_observation: durable_observation,
            ..
        } = &mut journal.state
        {
            durable_observation.clone_from(&control_observation);
        }
        if control_observation.is_some() {
            durability.persist_journal(&journal)?;
        }
    }
    let record = applied_record(
        &journal,
        &outcome,
        committed_state,
        ended_at,
        control_observation,
    )?;
    durability.persist_receipt(&record)?;
    durability.clear_journal()?;
    Ok(record.into_live_application_result(outcome, None))
}
