use tracedecay_application::{
    ApplicationOperation, CancellationObservation, CancellationStage, EffectTermination,
    ReconciliationState, SourceEditAuthorizationPort, SourceEditEffectRequestV1, SourceEditRequest,
    SourceEditVerificationStateV1, SourceEditVerificationV1, now_micros, source_edit_operation,
};
use tracedecay_domain::ManifestDigest;

use crate::tracedecay::TraceDecay;
use tracedecay_runtime_core::errors::Result;

use super::JOURNAL_VERSION;
use super::control::SourceEditEffectControlV1;
use super::digest::{
    effect_id, normalize_candidate_files, planned_source_edit_state_digest,
    source_edit_recovery_digest, source_edit_state_digest,
};
use super::dispatch::run_source_edit;
use super::journal::{
    ResolvedSourceEditPreview, SourceEditDurability, SourceEditDurableRequestV1,
    SourceEditJournalStateV1, SourceEditJournalV1, same_source_edit_authority,
};
use super::outcome::{SourceEditApplicationResult, SourceEditDurableOutcomeV1, SourceEditOutcome};
use super::reconcile::{recover_or_replay, recover_source_edit_transaction};
use super::records::{applied_record, durable_record, interrupted_record, unknown_record};
use super::verify::{
    application_contract_error, application_problem, config_error, run_edit_verifications,
};

fn durable_request(
    operation: &ApplicationOperation,
    request: &SourceEditEffectRequestV1,
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
        dry_run: request.edit.dry_run(),
        verification_requested: request.edit.verify(),
    }
}

/// The state triple every pre-effect receipt records about the attempted edit.
///
/// These three always travel together: the expected state the request was
/// admitted against, the predicted post-edit state when a preview produced
/// one, and the candidate files the edit would touch.
struct PreEffectState {
    expected: ManifestDigest,
    predicted: Option<ManifestDigest>,
    candidate_files: Vec<String>,
}

impl PreEffectState {
    /// The state of an edit rejected before any preview ran.
    fn unpreviewed(expected: ManifestDigest) -> Self {
        Self {
            expected,
            predicted: None,
            candidate_files: Vec::new(),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn persist_pre_effect_result(
    durability: &SourceEditDurability,
    operation: &ApplicationOperation,
    request: &SourceEditEffectRequestV1,
    authority: &tracedecay_application::SourceEditAuthorizationAdmissionV1,
    input_digest: &ManifestDigest,
    outcome: SourceEditOutcome,
    state: PreEffectState,
    termination: EffectTermination,
    control_observation: Option<CancellationObservation>,
) -> Result<SourceEditApplicationResult> {
    let PreEffectState {
        expected: expected_state,
        predicted: predicted_state,
        candidate_files,
    } = state;
    if let Some(stored) = durability.load_receipt(&request.idempotency_key)? {
        if stored.input_digest != *input_digest {
            return Err(config_error(
                "source edit idempotency key conflicts with a prior input",
            ));
        }
        if stored.authority_proof != authority.proof {
            return Err(config_error("source edit receipt authority changed"));
        }
        return Ok(stored.into_application_result(true));
    }
    let journal = SourceEditJournalV1 {
        version: JOURNAL_VERSION,
        effect_id: effect_id(&request.idempotency_key, input_digest)?,
        input_digest: input_digest.clone(),
        expected_state: expected_state.clone(),
        predicted_state,
        candidate_files,
        recovery_files: Vec::new(),
        recovery_digest: None,
        request: durable_request(operation, request, authority),
        state: SourceEditJournalStateV1::Prepared,
    };
    let committed_state = (termination == EffectTermination::Completed).then_some(expected_state);
    let record = durable_record(
        &journal,
        SourceEditDurableOutcomeV1::from_live(operation.use_case_id(), &outcome),
        committed_state,
        now_micros(),
        termination,
        ReconciliationState::Reconciled,
        control_observation,
    )?;
    durability.persist_receipt(&record)?;
    Ok(record.into_live_application_result(outcome, None))
}

fn failed_pre_effect_outcome() -> SourceEditOutcome {
    SourceEditOutcome::Failed {
        message: "source edit failed before the effect".to_owned(),
    }
}

/// Record a pre-effect failure: the one termination shape every guard between
/// admission and the effect uses.
fn fail_pre_effect(
    durability: &SourceEditDurability,
    operation: &ApplicationOperation,
    request: &SourceEditEffectRequestV1,
    authority: &tracedecay_application::SourceEditAuthorizationAdmissionV1,
    input_digest: &ManifestDigest,
    state: PreEffectState,
) -> Result<SourceEditApplicationResult> {
    persist_pre_effect_result(
        durability,
        operation,
        request,
        authority,
        input_digest,
        failed_pre_effect_outcome(),
        state,
        EffectTermination::Failed,
        None,
    )
}

/// The live outcome for a control stop, with the stage-specific wording.
fn control_stop_outcome(
    termination: EffectTermination,
    cancelled_message: &str,
    timed_out_message: &str,
) -> SourceEditOutcome {
    match termination {
        EffectTermination::Cancelled => SourceEditOutcome::Cancelled {
            message: cancelled_message.to_owned(),
        },
        EffectTermination::TimedOut => SourceEditOutcome::TimedOut {
            message: timed_out_message.to_owned(),
        },
        _ => unreachable!("source edit control only yields cancellation or timeout"),
    }
}

/// Retain the journal and report an unreconciled effect.
///
/// The journal stays `Prepared` on purpose: the effect may have crossed its
/// atomic rename boundary, so reconciliation — never an implicit retry — owns
/// the outcome.
fn persist_unknown(
    durability: &SourceEditDurability,
    journal: &SourceEditJournalV1,
    live_outcome: SourceEditOutcome,
    verification: Option<SourceEditVerificationV1>,
) -> Result<SourceEditApplicationResult> {
    let record = unknown_record(journal)?;
    durability.persist_receipt(&record)?;
    Ok(record.into_live_application_result(live_outcome, verification))
}

/// Whether a rechecked admission still carries the receipt and proof the
/// request was admitted with.
fn authority_still_matches(
    authority: &tracedecay_application::SourceEditAuthorizationAdmissionV1,
    request: &SourceEditEffectRequestV1,
) -> bool {
    same_source_edit_authority(&authority.receipt, &request.authority)
        && authority.proof == request.proof
}

pub(super) async fn execute_source_edit_inner<A>(
    graph: &TraceDecay,
    operation: &ApplicationOperation,
    request: SourceEditEffectRequestV1,
    authorization: &A,
    control: Option<&SourceEditEffectControlV1>,
) -> Result<SourceEditApplicationResult>
where
    A: SourceEditAuthorizationPort,
{
    request.validate().map_err(application_contract_error)?;
    let expected =
        source_edit_operation(request.edit.kind()).map_err(application_contract_error)?;
    if operation != &expected {
        return Err(config_error(
            "source edit request does not match its catalog operation",
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
        let outcome = control_stop_outcome(
            stop.termination,
            "source edit was cancelled before admission",
            "source edit timed out before admission",
        );
        return persist_pre_effect_result(
            &durability,
            operation,
            &request,
            &requested_authority,
            &input_digest,
            outcome,
            PreEffectState::unpreviewed(request.expected_state.clone()),
            stop.termination,
            Some(stop.observation),
        );
    }
    let admission = match authorization
        .admit(&request.context, operation, request.observed_at)
        .await
    {
        Ok(admission) => admission,
        Err(_) => {
            return fail_pre_effect(
                &durability,
                operation,
                &request,
                &requested_authority,
                &input_digest,
                PreEffectState::unpreviewed(request.expected_state.clone()),
            );
        }
    };
    if admission.receipt != request.authority || admission.proof != request.proof {
        return fail_pre_effect(
            &durability,
            operation,
            &request,
            &requested_authority,
            &input_digest,
            PreEffectState::unpreviewed(request.expected_state.clone()),
        );
    }
    let current_authority = match authorization
        .recheck_effect(&request.context, operation, &admission, now_micros())
        .await
    {
        Ok(authority) if authority_still_matches(&authority, &request) => authority,
        Ok(_) => {
            return fail_pre_effect(
                &durability,
                operation,
                &request,
                &requested_authority,
                &input_digest,
                PreEffectState::unpreviewed(request.expected_state.clone()),
            );
        }
        Err(error) => {
            if durability.load_receipt(&request.idempotency_key)?.is_some()
                || durability.load_journal()?.is_some()
            {
                return Err(application_problem(error));
            }
            return fail_pre_effect(
                &durability,
                operation,
                &request,
                &admission,
                &input_digest,
                PreEffectState::unpreviewed(request.expected_state.clone()),
            );
        }
    };
    recover_source_edit_transaction(&durability, graph, request.context.scope()).await?;
    if let Some(result) = recover_or_replay(&durability, &request, &input_digest)? {
        return Ok(result);
    }

    let preview = match resolve_source_edit_preview(graph, request.edit.clone()).await {
        Ok(preview) => preview,
        Err(_) => {
            return fail_pre_effect(
                &durability,
                operation,
                &request,
                &current_authority,
                &input_digest,
                PreEffectState::unpreviewed(request.expected_state.clone()),
            );
        }
    };
    if !preview.outcome.success() {
        return persist_pre_effect_result(
            &durability,
            operation,
            &request,
            &current_authority,
            &input_digest,
            preview.outcome,
            PreEffectState {
                expected: preview
                    .expected_state
                    .unwrap_or_else(|| request.expected_state.clone()),
                predicted: preview.predicted_state,
                candidate_files: preview.candidate_files,
            },
            EffectTermination::Failed,
            None,
        );
    }
    let predicted_state = preview
        .predicted_state
        .ok_or_else(|| config_error("successful source edit preview omitted predicted state"))?;
    let planned_files = preview.planned_files;
    let candidate_files = preview.candidate_files;
    let current_state = preview
        .expected_state
        .ok_or_else(|| config_error("successful source edit preview omitted expected state"))?;
    if !request.edit.dry_run() && current_state != request.expected_state {
        return fail_pre_effect(
            &durability,
            operation,
            &request,
            &current_authority,
            &input_digest,
            PreEffectState {
                expected: request.expected_state.clone(),
                predicted: Some(predicted_state),
                candidate_files,
            },
        );
    }
    if request.edit.dry_run() {
        if let Some(stop) =
            control.and_then(|control| control.checkpoint(CancellationStage::EffectInFlight))
        {
            let outcome = control_stop_outcome(
                stop.termination,
                "source edit preview was cancelled",
                "source edit preview timed out",
            );
            return persist_pre_effect_result(
                &durability,
                operation,
                &request,
                &current_authority,
                &input_digest,
                outcome,
                PreEffectState {
                    expected: current_state,
                    predicted: Some(predicted_state),
                    candidate_files,
                },
                stop.termination,
                Some(stop.observation),
            );
        }
        let current_authority = match authorization
            .recheck_effect(&request.context, operation, &admission, now_micros())
            .await
        {
            Ok(authority) if authority_still_matches(&authority, &request) => authority,
            _ => {
                return fail_pre_effect(
                    &durability,
                    operation,
                    &request,
                    &current_authority,
                    &input_digest,
                    PreEffectState {
                        expected: current_state,
                        predicted: Some(predicted_state),
                        candidate_files,
                    },
                );
            }
        };
        return persist_pre_effect_result(
            &durability,
            operation,
            &request,
            &current_authority,
            &input_digest,
            preview.outcome,
            PreEffectState {
                expected: current_state,
                predicted: Some(predicted_state),
                candidate_files,
            },
            EffectTermination::Completed,
            None,
        );
    }

    // Current authority and policy are checked again after every preview/read,
    // immediately before expected-state recapture and journal publication.
    let current_authority = match authorization
        .recheck_effect(&request.context, operation, &admission, now_micros())
        .await
    {
        Ok(authority) if authority_still_matches(&authority, &request) => authority,
        _ => {
            return fail_pre_effect(
                &durability,
                operation,
                &request,
                &current_authority,
                &input_digest,
                PreEffectState {
                    expected: request.expected_state.clone(),
                    predicted: Some(predicted_state),
                    candidate_files,
                },
            );
        }
    };
    let recaptured_state = match source_edit_state_digest(graph.project_root(), &candidate_files) {
        Ok(state) => state,
        Err(_) => {
            return fail_pre_effect(
                &durability,
                operation,
                &request,
                &current_authority,
                &input_digest,
                PreEffectState {
                    expected: request.expected_state.clone(),
                    predicted: Some(predicted_state),
                    candidate_files,
                },
            );
        }
    };
    if recaptured_state != request.expected_state {
        return fail_pre_effect(
            &durability,
            operation,
            &request,
            &current_authority,
            &input_digest,
            PreEffectState {
                expected: request.expected_state.clone(),
                predicted: Some(predicted_state),
                candidate_files,
            },
        );
    }

    let effect_id = effect_id(&request.idempotency_key, &input_digest)?;
    let durable_request = durable_request(operation, &request, &current_authority);
    let recovery_files = if planned_files
        .iter()
        .all(|file| file.expected.is_some() && file.intended.is_some())
    {
        planned_files.clone()
    } else {
        Vec::new()
    };
    let recovery_digest = (!recovery_files.is_empty())
        .then(|| source_edit_recovery_digest(&recovery_files))
        .transpose()?;
    let mut journal = SourceEditJournalV1 {
        version: JOURNAL_VERSION,
        effect_id,
        input_digest: input_digest.clone(),
        expected_state: request.expected_state.clone(),
        predicted_state: Some(predicted_state.clone()),
        candidate_files,
        recovery_files,
        recovery_digest,
        request: durable_request,
        state: SourceEditJournalStateV1::Prepared,
    };
    durability.persist_journal(&journal)?;

    if let Some(stop) =
        control.and_then(|control| control.checkpoint(CancellationStage::BeforeEffect))
    {
        let live_outcome = control_stop_outcome(
            stop.termination,
            "source edit was cancelled before the effect",
            "source edit timed out before the effect",
        );
        let record = interrupted_record(&journal, &live_outcome, stop)?;
        durability.persist_receipt(&record)?;
        durability.clear_journal()?;
        return Ok(record.into_live_application_result(live_outcome, None));
    }

    let (effect_result, plan_complete) = crate::tracedecay::apply_source_edit_plan(
        planned_files,
        run_source_edit(graph, request.edit.clone().with_dry_run(false), control),
    )
    .await;
    let mut outcome = match effect_result {
        Ok(outcome) => outcome,
        Err(error) => {
            // The edit primitive may have crossed its atomic rename boundary.
            // Retain Prepared and report EffectUnknown; never retry implicitly.
            let live_outcome = SourceEditOutcome::EffectUnknown {
                message: format!(
                    "source edit effect is unknown and requires reconciliation: {}",
                    error.to_string().chars().take(1024).collect::<String>()
                ),
            };
            return persist_unknown(&durability, &journal, live_outcome, None);
        }
    };
    let mut control_observation = control
        .and_then(|control| control.checkpoint(CancellationStage::EffectInFlight))
        .map(|stop| stop.observation);
    let mut committed_state =
        source_edit_state_digest(graph.project_root(), &journal.candidate_files)?;
    if outcome.success() && (!plan_complete || committed_state != predicted_state) {
        let live_outcome = SourceEditOutcome::EffectUnknown {
            message: "source edit effect is unknown and requires reconciliation: the observed committed state did not match the exact preview".to_owned(),
        };
        return persist_unknown(&durability, &journal, live_outcome, None);
    }
    if !outcome.success() && committed_state != journal.expected_state {
        let live_outcome = SourceEditOutcome::EffectUnknown {
            message: "source edit effect is unknown and requires reconciliation: the edit reported failure after candidate state changed".to_owned(),
        };
        return persist_unknown(&durability, &journal, live_outcome, None);
    }
    let verification = if request.edit.verify() && outcome.success() {
        let files = outcome.candidate_files();
        if files.is_empty() {
            None
        } else {
            Some(run_edit_verifications(graph, &files).await)
        }
    } else {
        None
    };
    if verification
        .as_ref()
        .is_some_and(|result| !matches!(result.state, SourceEditVerificationStateV1::Clean))
        && let (
            SourceEditRequest::ApiMigrationApply { plan, .. },
            SourceEditOutcome::ApiMigration(result),
        ) = (&request.edit, &mut outcome)
    {
        graph.rollback_api_migration_plan(plan).await?;
        result.success = false;
        result.rolled_back = true;
        result.changed_files.clear();
        "API migration verification did not pass; every changed file was restored"
            .clone_into(&mut result.message);
        committed_state = source_edit_state_digest(graph.project_root(), &journal.candidate_files)?;
        if committed_state != journal.expected_state {
            let live_outcome = SourceEditOutcome::EffectUnknown {
                message: "API migration verification rollback did not restore the previewed state"
                    .to_owned(),
            };
            return persist_unknown(&durability, &journal, live_outcome, verification);
        }
    }

    let ended_at = now_micros();
    journal.state = SourceEditJournalStateV1::Applied {
        outcome: SourceEditDurableOutcomeV1::from_live(&journal.request.operation, &outcome),
        committed_state: committed_state.clone(),
        ended_at,
        control_observation: control_observation.clone(),
        verification_state: None,
    };
    durability.persist_journal(&journal)?;

    if let SourceEditJournalStateV1::Applied {
        verification_state, ..
    } = &mut journal.state
    {
        *verification_state = verification.as_ref().map(|result| result.state);
    }
    if request.edit.verify() {
        durability.persist_journal(&journal)?;
    }
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
    Ok(record.into_live_application_result(outcome, verification))
}

pub(super) async fn resolve_source_edit_preview(
    graph: &TraceDecay,
    edit: SourceEditRequest,
) -> Result<ResolvedSourceEditPreview> {
    let (outcome, planned_files) = crate::tracedecay::capture_source_edit_plan(run_source_edit(
        graph,
        edit.with_dry_run(true),
        None,
    ))
    .await;
    let outcome = outcome?;
    if !outcome.success() {
        return Ok(ResolvedSourceEditPreview {
            outcome,
            candidate_files: Vec::new(),
            expected_state: None,
            predicted_state: None,
            planned_files: Vec::new(),
        });
    }
    let candidate_files =
        normalize_candidate_files(graph.project_root(), outcome.candidate_files())?;
    let planned_candidate_files = normalize_candidate_files(
        graph.project_root(),
        planned_files
            .iter()
            .map(|file| file.relative_path.clone())
            .collect(),
    )?;
    if planned_files.len() != candidate_files.len() || planned_candidate_files != candidate_files {
        return Err(config_error(
            "source edit preview did not produce one exact plan for every candidate file",
        ));
    }
    let expected_state = planned_source_edit_state_digest(&candidate_files, &planned_files, false)?;
    let observed_state = source_edit_state_digest(graph.project_root(), &candidate_files)?;
    if observed_state != expected_state {
        return Err(config_error(
            "source edit candidate state changed while its exact preview was captured",
        ));
    }
    let predicted_state = planned_source_edit_state_digest(&candidate_files, &planned_files, true)?;
    Ok(ResolvedSourceEditPreview {
        outcome,
        candidate_files,
        expected_state: Some(expected_state),
        predicted_state: Some(predicted_state),
        planned_files,
    })
}
