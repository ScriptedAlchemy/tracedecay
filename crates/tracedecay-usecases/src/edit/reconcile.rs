use std::path::Path;

use tracedecay_application::{
    ApplicationOperation, CancellationStage, EffectTermination, ReconciliationState,
    SourceEditAuthorizationPort, SourceEditEffectRequestV1, SourceEditReconciliationDispositionV1,
    SourceEditReconciliationRequestV1, now_micros, source_edit_operation,
    source_edit_reconciliation_operation,
};
use tracedecay_domain::ManifestDigest;

use crate::tracedecay::TraceDecay;
use tracedecay_runtime_core::errors::Result;

use super::JOURNAL_VERSION;
use super::control::SourceEditEffectControlV1;
use super::digest::source_edit_state_digest;
use super::journal::{
    SourceEditDurability, SourceEditJournalStateV1, SourceEditJournalV1, same_source_edit_authority,
};
use super::outcome::{SourceEditApplicationResult, SourceEditDurableOutcomeV1, SourceEditOutcome};
use super::records::{
    applied_durable_record, applied_record, durable_record,
    persist_interrupted_reconciliation_attempt, reconciliation_attempt_record, unknown_record,
};
use super::verify::{application_contract_error, application_problem, config_error};

pub(super) async fn reconcile_source_edit_effect_unknown_inner<A>(
    graph: &TraceDecay,
    request: SourceEditReconciliationRequestV1,
    authorization: &A,
    control: Option<&SourceEditEffectControlV1>,
) -> Result<SourceEditApplicationResult>
where
    A: SourceEditAuthorizationPort,
{
    request.validate().map_err(application_contract_error)?;
    let attempt_input_digest = request
        .attempt_input_digest()
        .map_err(application_contract_error)?;
    let reconciliation_operation =
        source_edit_reconciliation_operation().map_err(application_contract_error)?;
    let original_operation =
        source_edit_operation(request.kind).map_err(application_contract_error)?;
    let durability = SourceEditDurability::for_graph(graph);
    let _lock = durability.lock()?;
    if let Some(stored) =
        recover_reconciliation_attempt(&durability, &request, &attempt_input_digest)?
    {
        return Ok(stored);
    }
    if let Some(stop) =
        control.and_then(|control| control.checkpoint(CancellationStage::BeforeAdmission))
    {
        let journal = durability
            .load_journal()?
            .ok_or_else(|| config_error("no source edit effect requires reconciliation"))?;
        if journal.version != JOURNAL_VERSION
            || journal.effect_id != request.effect_id
            || journal.request.idempotency_key != request.idempotency_key
            || journal.input_digest != request.input_digest
            || journal.request.operation != *original_operation.use_case_id()
            || journal.request.actor != *request.context.actor()
            || journal.request.scope != *request.context.scope()
            || !matches!(journal.state, SourceEditJournalStateV1::Prepared)
        {
            return Err(config_error(
                "source edit reconciliation identity does not match the retained effect",
            ));
        }
        let authority = tracedecay_application::SourceEditAuthorizationAdmissionV1::new(
            request.authority.clone(),
            request.proof.clone(),
            request.context.scope(),
        )
        .map_err(application_contract_error)?;
        let attempt = SourceEditReconciliationAttemptV1 {
            operation: &reconciliation_operation,
            authority: &authority,
            input_digest: &attempt_input_digest,
            control,
        };
        return persist_interrupted_reconciliation_attempt(
            &durability,
            &journal,
            &request,
            &attempt,
            stop,
        );
    }
    let admission = authorization
        .admit(
            &request.context,
            &reconciliation_operation,
            request.observed_at,
        )
        .await
        .map_err(application_problem)?;
    if admission.receipt != request.authority || admission.proof != request.proof {
        return Err(config_error(
            "source edit reconciliation admission differs from its authority receipt",
        ));
    }
    let current_authority = authorization
        .recheck_effect(
            &request.context,
            &reconciliation_operation,
            &admission,
            now_micros(),
        )
        .await
        .map_err(application_problem)?;
    if !same_source_edit_authority(&current_authority.receipt, &request.authority)
        || current_authority.proof != request.proof
    {
        return Err(config_error(
            "source edit reconciliation current authority changed",
        ));
    }

    reconcile_prepared_source_edit_controlled(
        &durability,
        graph.project_root(),
        &original_operation,
        request,
        Some(SourceEditReconciliationAttemptV1 {
            operation: &reconciliation_operation,
            authority: &current_authority,
            input_digest: &attempt_input_digest,
            control,
        }),
    )
}

fn recover_reconciliation_attempt(
    durability: &SourceEditDurability,
    request: &SourceEditReconciliationRequestV1,
    attempt_input_digest: &ManifestDigest,
) -> Result<Option<SourceEditApplicationResult>> {
    let Some(stored) = durability.load_reconciliation_receipt(&request.attempt_idempotency_key)?
    else {
        return Ok(None);
    };
    if stored.input_digest != *attempt_input_digest {
        return Err(config_error(
            "source edit reconciliation attempt idempotency key conflicts with a prior input",
        ));
    }
    if stored.authority_proof != request.proof
        || !same_source_edit_authority(&stored.effect.authority, &request.authority)
    {
        return Err(config_error(
            "source edit reconciliation replay authority changed",
        ));
    }
    if stored.effect.receipt.outcome == EffectTermination::Completed {
        let original = durability
            .load_receipt(&request.idempotency_key)?
            .ok_or_else(|| {
                config_error(
                    "completed reconciliation attempt is missing its original effect receipt",
                )
            })?;
        if original.input_digest != request.input_digest
            || original.effect.reconciliation != ReconciliationState::Reconciled
            || original.effect.receipt.outcome == EffectTermination::EffectUnknown
        {
            return Err(config_error(
                "completed reconciliation attempt does not match a terminal original effect",
            ));
        }
        if let Some(journal) = durability.load_journal()?
            && journal.effect_id == request.effect_id
            && journal.request.idempotency_key == request.idempotency_key
            && journal.input_digest == request.input_digest
            && matches!(journal.state, SourceEditJournalStateV1::Prepared)
        {
            durability.clear_journal()?;
        }
    }
    Ok(Some(stored.into_application_result(true)))
}

pub(super) struct SourceEditReconciliationAttemptV1<'a> {
    pub(super) operation: &'a ApplicationOperation,
    pub(super) authority: &'a tracedecay_application::SourceEditAuthorizationAdmissionV1,
    pub(super) input_digest: &'a ManifestDigest,
    pub(super) control: Option<&'a SourceEditEffectControlV1>,
}

fn retained_reconciliation_journal(
    durability: &SourceEditDurability,
    operation: &ApplicationOperation,
    request: &SourceEditReconciliationRequestV1,
) -> Result<SourceEditJournalV1> {
    let journal = durability
        .load_journal()?
        .ok_or_else(|| config_error("no source edit effect requires reconciliation"))?;
    if journal.version != JOURNAL_VERSION
        || journal.effect_id != request.effect_id
        || journal.request.idempotency_key != request.idempotency_key
        || journal.input_digest != request.input_digest
        || &journal.request.operation != operation.use_case_id()
        || &journal.request.actor != request.context.actor()
        || &journal.request.scope != request.context.scope()
    {
        return Err(config_error(
            "source edit reconciliation identity does not match the retained effect",
        ));
    }
    if !matches!(journal.state, SourceEditJournalStateV1::Prepared) {
        return Err(config_error(
            "source edit effect already has a durable applied-state proof",
        ));
    }
    Ok(journal)
}

#[cfg(test)]
fn reconcile_prepared_source_edit(
    durability: &SourceEditDurability,
    project_root: &Path,
    operation: &ApplicationOperation,
    request: SourceEditReconciliationRequestV1,
) -> Result<SourceEditApplicationResult> {
    reconcile_prepared_source_edit_controlled(durability, project_root, operation, request, None)
}

fn reconcile_prepared_source_edit_controlled(
    durability: &SourceEditDurability,
    project_root: &Path,
    operation: &ApplicationOperation,
    request: SourceEditReconciliationRequestV1,
    attempt: Option<SourceEditReconciliationAttemptV1<'_>>,
) -> Result<SourceEditApplicationResult> {
    let journal = retained_reconciliation_journal(durability, operation, &request)?;
    if let Some(attempt) = &attempt
        && let Some(stop) = attempt
            .control
            .and_then(|control| control.checkpoint(CancellationStage::BeforeEffect))
    {
        return persist_interrupted_reconciliation_attempt(
            durability, &journal, &request, attempt, stop,
        );
    }
    let observed_state = source_edit_state_digest(project_root, &journal.candidate_files)?;
    if let Some(attempt) = &attempt
        && let Some(stop) = attempt
            .control
            .and_then(|control| control.checkpoint(CancellationStage::EffectInFlight))
    {
        return persist_interrupted_reconciliation_attempt(
            durability, &journal, &request, attempt, stop,
        );
    }
    let ended_at = now_micros();
    let (_outcome, record) = match request.disposition.clone() {
        SourceEditReconciliationDispositionV1::ConfirmCommitted { committed_state } => {
            let predicted_state = journal.predicted_state.as_ref().ok_or_else(|| {
                config_error(
                    "source edit committed state cannot be proven from this legacy journal",
                )
            })?;
            if &committed_state != predicted_state || observed_state != *predicted_state {
                return Err(config_error(
                    "source edit committed-state inspection does not match the exact preview",
                ));
            }
            let outcome = SourceEditOutcome::Reconciled {
                success: true,
                message: "source edit effect was independently confirmed committed".to_owned(),
            };
            let record = applied_record(&journal, &outcome, committed_state, ended_at, None)?;
            (outcome, record)
        }
        SourceEditReconciliationDispositionV1::ConfirmRolledBack => {
            if observed_state != journal.expected_state {
                return Err(config_error(
                    "source edit rollback inspection does not match the admitted expected state",
                ));
            }
            let outcome = SourceEditOutcome::Reconciled {
                success: false,
                message: "source edit effect was independently confirmed rolled back".to_owned(),
            };
            let record = durable_record(
                &journal,
                SourceEditDurableOutcomeV1::from_live(&journal.request.operation, &outcome),
                None,
                ended_at,
                EffectTermination::Failed,
                ReconciliationState::Reconciled,
                None,
            )?;
            (outcome, record)
        }
    };
    durability.persist_receipt(&record)?;
    let result = if let Some(attempt) = attempt {
        let after_commit_observation = attempt
            .control
            .and_then(|control| control.checkpoint(CancellationStage::AfterCommit))
            .map(|stop| stop.observation);
        let attempt_outcome = SourceEditOutcome::Reconciled {
            success: true,
            message: "source edit reconciliation attempt completed".to_owned(),
        };
        let attempt_record = reconciliation_attempt_record(
            &journal,
            &request,
            &attempt,
            &attempt_outcome,
            record.effect.receipt.committed_state.clone(),
            ended_at,
            EffectTermination::Completed,
            after_commit_observation,
        )?;
        durability.persist_reconciliation_receipt(&attempt_record)?;
        attempt_record.into_live_application_result(attempt_outcome, None)
    } else {
        record.into_application_result(true)
    };
    durability.clear_journal()?;
    Ok(result)
}

pub(super) async fn recover_source_edit_transaction(
    durability: &SourceEditDurability,
    graph: &TraceDecay,
    scope: &tracedecay_application::ResolvedScope,
) -> Result<()> {
    let Some(journal) = durability.load_journal()? else {
        return Ok(());
    };
    if &journal.request.scope != scope {
        return Err(config_error(
            "source edit crash recovery must run in the transaction's owning worktree",
        ));
    }
    if let SourceEditJournalStateV1::Applied {
        outcome,
        committed_state,
        ended_at,
        control_observation,
        verification_state,
    } = &journal.state
    {
        let record = applied_durable_record(
            &journal,
            outcome.clone(),
            committed_state.clone(),
            *ended_at,
            control_observation.clone(),
            *verification_state,
        )?;
        durability.persist_receipt(&record)?;
        return durability.clear_journal();
    }
    if journal.recovery_files.is_empty() {
        return Ok(());
    }
    // The journal is `Prepared` with captured preimages: the durable commit
    // never finalized. But the edit primitive publishes every file to disk
    // BEFORE the journal advances to `Applied`, so the worktree may already hold
    // the finished edit. Inspect the on-disk state before touching a single
    // byte — a crash after a successful write must never silently roll that
    // written edit back to its preimage. This mirrors the client-timeout path,
    // which surfaces `EffectUnknown` and lets `source_edit_reconcile` decide,
    // rather than destroying bytes.
    let observed_state = source_edit_state_digest(graph.project_root(), &journal.candidate_files)?;

    // (i) Roll forward. The worktree already holds the exact previewed result,
    //     so the write succeeded and only the bookkeeping was lost. Finalize the
    //     commit and keep every byte; the client-timeout `ConfirmCommitted`
    //     disposition reaches the same durable record. `recovery_files` is only
    //     ever populated alongside `predicted_state` (see `execute.rs`), so a
    //     present predicted state is guaranteed here.
    if journal.predicted_state.as_ref() == Some(&observed_state) {
        graph
            .commit_source_edit_postimages(&journal.recovery_files)
            .await?;
        let outcome = SourceEditOutcome::Reconciled {
            success: true,
            message: "source edit crash recovery confirmed the edit already committed to disk"
                .to_owned(),
        };
        let record = applied_record(&journal, &outcome, observed_state, now_micros(), None)?;
        durability.persist_receipt(&record)?;
        return durability.clear_journal();
    }

    // (ii)/(iii) The write did not complete. When the worktree is still fully at
    //     the preimage (ii) the write never landed and rollback is a no-op. When
    //     it is a torn partial multi-file write (iii) — some files published,
    //     others not — rolling back to a consistent pre-edit state lets the whole
    //     atomic plan be retried, but it discards the bytes of the files that did
    //     publish, so we WARN first. `recover_source_edit_preimages` restores
    //     per file and REFUSES any foreign bytes outright, so it can only ever
    //     touch files it can prove hold either the preimage or the intended edit;
    //     genuinely unaccountable content fails recovery instead of being erased.
    if observed_state != journal.expected_state {
        tracing::warn!(
            target: "tracedecay::source_edit::recovery",
            effect_id = %journal.effect_id.as_str(),
            files = ?journal.candidate_files,
            "source edit crash recovery is rolling a torn partial write back to its \
             preimage; on-disk edited bytes that did not match the completed preview \
             are being discarded"
        );
    }
    graph
        .recover_source_edit_preimages(&journal.recovery_files)
        .await?;
    let restored_state = source_edit_state_digest(graph.project_root(), &journal.candidate_files)?;
    if restored_state != journal.expected_state {
        return Err(config_error(
            "source edit crash recovery did not restore the journaled preimage state",
        ));
    }
    let outcome = SourceEditOutcome::Failed {
        message: "source edit crash recovery restored every journaled preimage".to_owned(),
    };
    let mut durable_outcome =
        SourceEditDurableOutcomeV1::from_live(&journal.request.operation, &outcome);
    durable_outcome.files.clone_from(&journal.candidate_files);
    let record = durable_record(
        &journal,
        durable_outcome,
        None,
        now_micros(),
        EffectTermination::Failed,
        ReconciliationState::Reconciled,
        None,
    )?;
    durability.persist_receipt(&record)?;
    durability.clear_journal()
}

pub(super) fn recover_or_replay(
    durability: &SourceEditDurability,
    request: &SourceEditEffectRequestV1,
    input_digest: &ManifestDigest,
) -> Result<Option<SourceEditApplicationResult>> {
    if let Some(stored) = durability.load_receipt(&request.idempotency_key)? {
        if stored.input_digest != *input_digest {
            return Err(config_error(
                "source edit idempotency key conflicts with a prior input",
            ));
        }
        if stored.authority_proof != request.proof
            || !same_source_edit_authority(&stored.effect.authority, &request.authority)
        {
            return Err(config_error("source edit replay authority changed"));
        }
        if let Some(journal) = durability.load_journal()?
            && journal.request.idempotency_key == request.idempotency_key
            && journal.input_digest == *input_digest
            && matches!(journal.state, SourceEditJournalStateV1::Applied { .. })
        {
            durability.clear_journal()?;
        }
        return Ok(Some(stored.into_application_result(true)));
    }
    durability
        .load_journal()?
        .map(|journal| reconcile_journal(durability, journal, request, input_digest))
        .transpose()
}

fn reconcile_journal(
    durability: &SourceEditDurability,
    journal: SourceEditJournalV1,
    request: &SourceEditEffectRequestV1,
    input_digest: &ManifestDigest,
) -> Result<SourceEditApplicationResult> {
    if journal.version != JOURNAL_VERSION {
        return Err(config_error(
            "unsupported source edit transaction journal version",
        ));
    }
    if journal.request.idempotency_key != request.idempotency_key
        || journal.input_digest != *input_digest
        || !same_source_edit_authority(&journal.request.authority, &request.authority)
        || journal.request.authority_proof != request.proof
    {
        return Err(config_error(
            "a source edit transaction requires reconciliation before another mutation",
        ));
    }
    let record = match &journal.state {
        SourceEditJournalStateV1::Prepared => unknown_record(&journal)?,
        SourceEditJournalStateV1::Applied {
            outcome,
            committed_state,
            ended_at,
            control_observation,
            verification_state,
        } => applied_durable_record(
            &journal,
            outcome.clone(),
            committed_state.clone(),
            *ended_at,
            control_observation.clone(),
            *verification_state,
        )?,
    };
    durability.persist_receipt(&record)?;
    if matches!(journal.state, SourceEditJournalStateV1::Applied { .. }) {
        durability.clear_journal()?;
    }
    Ok(record.into_application_result(true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::test_support::*;

    use crate::edit::digest::{planned_source_edit_state_digest, source_edit_recovery_digest};
    use std::fs;
    use tempfile::tempdir;
    use tracedecay_application::source_edit::EditResult;
    use tracedecay_application::{CancellationSignal, Deadline};
    use tracedecay_domain::UtcMicros;

    #[test]
    fn prepared_restart_is_durable_effect_unknown_and_not_replayed() {
        let directory = tempdir().unwrap();
        let durability = SourceEditDurability {
            root: directory.path().to_path_buf(),
        };
        let request = fixture_request();
        let journal = fixture_journal(&request, SourceEditJournalStateV1::Prepared);
        durability.persist_journal(&journal).unwrap();

        let result = reconcile_journal(
            &durability,
            durability.load_journal().unwrap().unwrap(),
            &request,
            &request.input_digest().unwrap(),
        )
        .unwrap();

        assert_eq!(
            result.effect.unwrap().receipt.outcome,
            EffectTermination::EffectUnknown
        );
        assert!(
            durability.load_journal().unwrap().is_some(),
            "an unknown prepared effect must retain its recovery evidence"
        );
    }

    #[test]
    fn prepared_restart_rejects_preimages_that_do_not_match_the_journal_digest() {
        let directory = tempdir().unwrap();
        let durability = SourceEditDurability {
            root: directory.path().to_path_buf(),
        };
        let request = fixture_request();
        let mut journal = fixture_journal(&request, SourceEditJournalStateV1::Prepared);
        journal.recovery_files = vec![crate::tracedecay::PlannedSourceEditFile {
            relative_path: "src/lib.rs".to_owned(),
            expected: Some("old".to_owned()),
            intended: Some("new".to_owned()),
        }];
        journal.recovery_digest =
            Some(source_edit_recovery_digest(&journal.recovery_files).unwrap());
        journal.recovery_files[0].expected = Some("tampered".to_owned());
        durability.persist_journal(&journal).unwrap();

        assert!(durability.load_journal().is_err());
    }

    #[test]
    fn applied_restart_finalizes_original_receipt_and_clears_journal() {
        let directory = tempdir().unwrap();
        let durability = SourceEditDurability {
            root: directory.path().to_path_buf(),
        };
        let request = fixture_request();
        let outcome = SourceEditOutcome::Edit(EditResult {
            success: true,
            file_path: "src/lib.rs".to_owned(),
            message: "applied".to_owned(),
            ..EditResult::default()
        });
        let journal = fixture_journal(
            &request,
            SourceEditJournalStateV1::Applied {
                outcome: SourceEditDurableOutcomeV1::from_live(
                    source_edit_operation(request.edit.kind())
                        .unwrap()
                        .use_case_id(),
                    &outcome,
                ),
                committed_state: digest(SHA256_B),
                ended_at: UtcMicros(4),
                control_observation: None,
                verification_state: None,
            },
        );
        durability.persist_journal(&journal).unwrap();

        let result = reconcile_journal(
            &durability,
            durability.load_journal().unwrap().unwrap(),
            &request,
            &request.input_digest().unwrap(),
        )
        .unwrap();

        assert_eq!(
            result.effect.unwrap().receipt.outcome,
            EffectTermination::Completed
        );
        assert!(durability.load_journal().unwrap().is_none());
        assert!(
            durability
                .load_receipt(&request.idempotency_key)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn replay_rejects_authority_drift_and_same_key_changed_input() {
        let directory = tempdir().unwrap();
        let durability = SourceEditDurability {
            root: directory.path().to_path_buf(),
        };
        let request = fixture_request();
        let outcome = SourceEditOutcome::Edit(EditResult {
            success: true,
            file_path: "src/lib.rs".to_owned(),
            message: "applied".to_owned(),
            ..EditResult::default()
        });
        let journal = fixture_journal(
            &request,
            SourceEditJournalStateV1::Applied {
                outcome: SourceEditDurableOutcomeV1::from_live(
                    source_edit_operation(request.edit.kind())
                        .unwrap()
                        .use_case_id(),
                    &outcome,
                ),
                committed_state: digest(SHA256_B),
                ended_at: UtcMicros(4),
                control_observation: None,
                verification_state: None,
            },
        );
        let record =
            applied_record(&journal, &outcome, digest(SHA256_B), UtcMicros(4), None).unwrap();
        durability.persist_receipt(&record).unwrap();

        let replay = recover_or_replay(&durability, &request, &request.input_digest().unwrap())
            .unwrap()
            .unwrap();
        assert!(replay.replayed);
        assert_eq!(
            replay.effect.unwrap().receipt.outcome,
            EffectTermination::Completed
        );

        let mut current_proof = request.clone();
        current_proof.proof.configuration_digest = digest(SHA256_B);
        current_proof.proof.configuration_revision_id =
            tracedecay_domain::configuration::ConfigurationRevisionId::new(
                "configuration.edit.fixture.v2",
            )
            .unwrap();
        current_proof.proof.catalog_revision = 2;
        current_proof.proof.catalog_digest = digest(SHA256_B);
        current_proof.proof.privacy_key_epoch = 2;
        current_proof.proof.privacy_digest = digest(SHA256_B);
        assert_eq!(
            current_proof.input_digest().unwrap(),
            request.input_digest().unwrap()
        );
        assert!(
            recover_or_replay(
                &durability,
                &current_proof,
                &current_proof.input_digest().unwrap(),
            )
            .is_err()
        );

        let mut conflict = request.clone();
        conflict.expected_state = digest(SHA256_B);
        assert!(
            recover_or_replay(&durability, &conflict, &conflict.input_digest().unwrap()).is_err()
        );
    }

    #[test]
    fn cancelled_reconciliation_attempt_is_separate_replayable_and_conflict_safe() {
        let directory = tempdir().unwrap();
        let durability = SourceEditDurability {
            root: directory.path().to_path_buf(),
        };
        let request = fixture_request();
        let journal = fixture_journal(&request, SourceEditJournalStateV1::Prepared);
        durability.persist_journal(&journal).unwrap();
        durability
            .persist_receipt(&unknown_record(&journal).unwrap())
            .unwrap();
        let reconciliation = fixture_reconciliation(
            &request,
            &journal,
            SourceEditReconciliationDispositionV1::ConfirmRolledBack,
        );
        let attempt_input = digest(SHA256_B);
        let operation = source_edit_reconciliation_operation().unwrap();
        let cancellation = CancellationSignal::active("cancel.reconcile.fixture").unwrap();
        assert!(cancellation.cancel(UtcMicros(5)));
        let control = SourceEditEffectControlV1::new(
            Deadline::new(UtcMicros(i64::MAX)).unwrap(),
            cancellation,
        );
        let reconciliation_authority =
            tracedecay_application::SourceEditAuthorizationAdmissionV1::new(
                reconciliation.authority.clone(),
                reconciliation.proof.clone(),
                reconciliation.context.scope(),
            )
            .unwrap();
        let attempt = SourceEditReconciliationAttemptV1 {
            operation: &operation,
            authority: &reconciliation_authority,
            input_digest: &attempt_input,
            control: Some(&control),
        };
        let result = reconcile_prepared_source_edit_controlled(
            &durability,
            directory.path(),
            &source_edit_operation(request.edit.kind()).unwrap(),
            reconciliation.clone(),
            Some(attempt),
        )
        .unwrap();

        assert_eq!(
            result.effect.unwrap().receipt.outcome,
            EffectTermination::Cancelled
        );
        assert_eq!(
            durability
                .load_receipt(&request.idempotency_key)
                .unwrap()
                .unwrap()
                .effect
                .receipt
                .outcome,
            EffectTermination::EffectUnknown
        );
        assert!(durability.load_journal().unwrap().is_some());
        assert!(
            recover_reconciliation_attempt(&durability, &reconciliation, &attempt_input)
                .unwrap()
                .unwrap()
                .replayed
        );
        assert!(
            recover_reconciliation_attempt(&durability, &reconciliation, &digest(SHA256_A))
                .is_err()
        );
    }

    #[test]
    fn completed_reconciliation_attempt_replay_clears_prepared_journal() {
        let directory = tempdir().unwrap();
        let durability = SourceEditDurability {
            root: directory.path().to_path_buf(),
        };
        let request = fixture_request();
        let journal = fixture_journal(&request, SourceEditJournalStateV1::Prepared);
        durability.persist_journal(&journal).unwrap();
        let reconciliation = fixture_reconciliation(
            &request,
            &journal,
            SourceEditReconciliationDispositionV1::ConfirmRolledBack,
        );
        let original_outcome = SourceEditOutcome::Reconciled {
            success: false,
            message: "rolled back".to_owned(),
        };
        let original = durable_record(
            &journal,
            SourceEditDurableOutcomeV1::from_live(&journal.request.operation, &original_outcome),
            None,
            UtcMicros(5),
            EffectTermination::Failed,
            ReconciliationState::Reconciled,
            None,
        )
        .unwrap();
        durability.persist_receipt(&original).unwrap();
        let attempt_input = digest(SHA256_B);
        let operation = source_edit_reconciliation_operation().unwrap();
        let reconciliation_authority =
            tracedecay_application::SourceEditAuthorizationAdmissionV1::new(
                reconciliation.authority.clone(),
                reconciliation.proof.clone(),
                reconciliation.context.scope(),
            )
            .unwrap();
        let attempt = SourceEditReconciliationAttemptV1 {
            operation: &operation,
            authority: &reconciliation_authority,
            input_digest: &attempt_input,
            control: None,
        };
        let attempt_outcome = SourceEditOutcome::Reconciled {
            success: true,
            message: "completed".to_owned(),
        };
        let completed = reconciliation_attempt_record(
            &journal,
            &reconciliation,
            &attempt,
            &attempt_outcome,
            None,
            UtcMicros(6),
            EffectTermination::Completed,
            None,
        )
        .unwrap();
        durability
            .persist_reconciliation_receipt(&completed)
            .unwrap();

        assert!(
            recover_reconciliation_attempt(&durability, &reconciliation, &attempt_input)
                .unwrap()
                .unwrap()
                .replayed
        );
        assert!(durability.load_journal().unwrap().is_none());
    }

    #[test]
    fn authorized_committed_reconciliation_replaces_unknown_and_unblocks_edits() {
        let project = tempdir().unwrap();
        fs::create_dir_all(project.path().join("src")).unwrap();
        fs::write(project.path().join("src/lib.rs"), b"before").unwrap();
        let durability = SourceEditDurability {
            root: project.path().join("durability"),
        };
        let files = vec!["src/lib.rs".to_owned()];
        let mut request = fixture_request();
        request.expected_state = source_edit_state_digest(project.path(), &files).unwrap();
        let mut journal = fixture_journal(&request, SourceEditJournalStateV1::Prepared);
        journal.predicted_state = Some(
            planned_source_edit_state_digest(
                &files,
                &[crate::tracedecay::PlannedSourceEditFile {
                    relative_path: "src/lib.rs".to_owned(),
                    expected: Some("before".to_owned()),
                    intended: Some("after".to_owned()),
                }],
                true,
            )
            .unwrap(),
        );
        durability.persist_journal(&journal).unwrap();
        let unknown = reconcile_journal(
            &durability,
            durability.load_journal().unwrap().unwrap(),
            &request,
            &request.input_digest().unwrap(),
        )
        .unwrap();
        assert_eq!(
            unknown.effect.unwrap().receipt.outcome,
            EffectTermination::EffectUnknown
        );

        fs::write(project.path().join("src/lib.rs"), b"after").unwrap();
        let committed_state = source_edit_state_digest(project.path(), &files).unwrap();
        let reconciliation = fixture_reconciliation(
            &request,
            &journal,
            SourceEditReconciliationDispositionV1::ConfirmCommitted {
                committed_state: committed_state.clone(),
            },
        );
        let operation = source_edit_operation(request.edit.kind()).unwrap();
        let resolved =
            reconcile_prepared_source_edit(&durability, project.path(), &operation, reconciliation)
                .unwrap();

        assert_eq!(resolved.predicted_state, Some(committed_state.clone()));
        assert_eq!(
            resolved.effect.unwrap().receipt.committed_state,
            Some(committed_state)
        );
        assert!(durability.load_journal().unwrap().is_none());
        assert!(
            durability
                .load_receipt(&request.idempotency_key)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn reconciliation_mismatch_retains_unknown_journal() {
        let project = tempdir().unwrap();
        fs::create_dir_all(project.path().join("src")).unwrap();
        fs::write(project.path().join("src/lib.rs"), b"before").unwrap();
        let durability = SourceEditDurability {
            root: project.path().join("durability"),
        };
        let files = vec!["src/lib.rs".to_owned()];
        let mut request = fixture_request();
        request.expected_state = source_edit_state_digest(project.path(), &files).unwrap();
        let mut journal = fixture_journal(&request, SourceEditJournalStateV1::Prepared);
        journal.predicted_state = Some(
            planned_source_edit_state_digest(
                &files,
                &[crate::tracedecay::PlannedSourceEditFile {
                    relative_path: "src/lib.rs".to_owned(),
                    expected: Some("before".to_owned()),
                    intended: Some("intended".to_owned()),
                }],
                true,
            )
            .unwrap(),
        );
        durability.persist_journal(&journal).unwrap();
        fs::write(project.path().join("src/lib.rs"), b"unrelated").unwrap();
        let unrelated_state = source_edit_state_digest(project.path(), &files).unwrap();
        let reconciliation = fixture_reconciliation(
            &request,
            &journal,
            SourceEditReconciliationDispositionV1::ConfirmCommitted {
                committed_state: unrelated_state,
            },
        );
        let operation = source_edit_operation(request.edit.kind()).unwrap();

        assert!(
            reconcile_prepared_source_edit(
                &durability,
                project.path(),
                &operation,
                reconciliation,
            )
            .is_err()
        );
        assert!(durability.load_journal().unwrap().is_some());
    }
}
