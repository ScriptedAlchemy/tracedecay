use tracedecay_application::{
    CancellationObservation, EffectReceipt, EffectResult, EffectTermination, OperationBudgetUsage,
    OperationReceipt, OperationTermination, ReconciliationState,
    SourceEditReconciliationDispositionV1, SourceEditReconciliationRequestV1,
    SourceEditVerificationStateV1, now_micros,
};
use tracedecay_domain::{ManifestDigest, UtcMicros};

use tracedecay_runtime_core::errors::Result;

use super::JOURNAL_VERSION;
use super::control::SourceEditControlStopV1;
use super::digest::reconciliation_attempt_effect_id;
use super::journal::{
    SourceEditDurability, SourceEditDurableResultV1, SourceEditJournalStateV1, SourceEditJournalV1,
};
use super::outcome::{SourceEditApplicationResult, SourceEditDurableOutcomeV1, SourceEditOutcome};
use super::reconcile::SourceEditReconciliationAttemptV1;
use super::verify::application_contract_error;

pub(super) fn applied_record(
    journal: &SourceEditJournalV1,
    outcome: &SourceEditOutcome,
    committed_state: ManifestDigest,
    ended_at: UtcMicros,
    control_observation: Option<CancellationObservation>,
) -> Result<SourceEditDurableResultV1> {
    let verification_state = match &journal.state {
        SourceEditJournalStateV1::Applied {
            verification_state, ..
        } => *verification_state,
        SourceEditJournalStateV1::Prepared => None,
    };
    let termination = applied_effect_termination(
        journal.request.verification_requested,
        verification_state,
        outcome.success(),
    );
    durable_record(
        journal,
        SourceEditDurableOutcomeV1::from_live(&journal.request.operation, outcome),
        Some(committed_state),
        ended_at,
        termination,
        ReconciliationState::Reconciled,
        control_observation,
    )
}

pub(super) fn applied_durable_record(
    journal: &SourceEditJournalV1,
    outcome: SourceEditDurableOutcomeV1,
    committed_state: ManifestDigest,
    ended_at: UtcMicros,
    control_observation: Option<CancellationObservation>,
    verification_state: Option<SourceEditVerificationStateV1>,
) -> Result<SourceEditDurableResultV1> {
    let termination = applied_effect_termination(
        journal.request.verification_requested,
        verification_state,
        outcome.success,
    );
    durable_record(
        journal,
        outcome,
        Some(committed_state),
        ended_at,
        termination,
        ReconciliationState::Reconciled,
        control_observation,
    )
}

fn applied_effect_termination(
    verification_requested: bool,
    verification_state: Option<SourceEditVerificationStateV1>,
    source_edit_succeeded: bool,
) -> EffectTermination {
    if !source_edit_succeeded {
        return EffectTermination::Failed;
    }
    if !verification_requested
        || matches!(
            verification_state,
            Some(SourceEditVerificationStateV1::Clean | SourceEditVerificationStateV1::Errors)
        )
    {
        EffectTermination::Completed
    } else {
        EffectTermination::Partial
    }
}

pub(super) fn unknown_record(journal: &SourceEditJournalV1) -> Result<SourceEditDurableResultV1> {
    let outcome = SourceEditOutcome::EffectUnknown {
        message: "source edit effect is unknown and requires reconciliation".to_owned(),
    };
    durable_record(
        journal,
        SourceEditDurableOutcomeV1::from_live(&journal.request.operation, &outcome),
        None,
        now_micros(),
        EffectTermination::EffectUnknown,
        ReconciliationState::Pending,
        None,
    )
}

pub(super) fn interrupted_record(
    journal: &SourceEditJournalV1,
    outcome: &SourceEditOutcome,
    stop: SourceEditControlStopV1,
) -> Result<SourceEditDurableResultV1> {
    durable_record(
        journal,
        SourceEditDurableOutcomeV1::from_live(&journal.request.operation, outcome),
        None,
        stop.observation.observed_at,
        stop.termination,
        ReconciliationState::Reconciled,
        Some(stop.observation),
    )
}

pub(super) fn persist_interrupted_reconciliation_attempt(
    durability: &SourceEditDurability,
    journal: &SourceEditJournalV1,
    request: &SourceEditReconciliationRequestV1,
    attempt: &SourceEditReconciliationAttemptV1<'_>,
    stop: SourceEditControlStopV1,
) -> Result<SourceEditApplicationResult> {
    let outcome = match stop.termination {
        EffectTermination::Cancelled => SourceEditOutcome::Cancelled {
            message: "source edit reconciliation attempt was cancelled".to_owned(),
        },
        EffectTermination::TimedOut => SourceEditOutcome::TimedOut {
            message: "source edit reconciliation attempt timed out".to_owned(),
        },
        _ => unreachable!("source edit control only yields cancellation or timeout"),
    };
    let record = reconciliation_attempt_record(
        journal,
        request,
        attempt,
        &outcome,
        None,
        stop.observation.observed_at,
        stop.termination,
        Some(stop.observation),
    )?;
    durability.persist_reconciliation_receipt(&record)?;
    Ok(record.into_live_application_result(outcome, None))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn reconciliation_attempt_record(
    journal: &SourceEditJournalV1,
    request: &SourceEditReconciliationRequestV1,
    attempt: &SourceEditReconciliationAttemptV1<'_>,
    outcome: &SourceEditOutcome,
    committed_state: Option<ManifestDigest>,
    ended_at: UtcMicros,
    termination: EffectTermination,
    control_observation: Option<CancellationObservation>,
) -> Result<SourceEditDurableResultV1> {
    let committed_state = if termination == EffectTermination::Completed {
        Some(match &request.disposition {
            SourceEditReconciliationDispositionV1::ConfirmCommitted { committed_state } => {
                committed_state.clone()
            }
            SourceEditReconciliationDispositionV1::ConfirmRolledBack => {
                journal.expected_state.clone()
            }
        })
    } else {
        committed_state
    };
    let operation_termination = match termination {
        EffectTermination::Completed => OperationTermination::Completed,
        EffectTermination::Cancelled => OperationTermination::Cancelled,
        EffectTermination::TimedOut => OperationTermination::TimedOut,
        EffectTermination::Failed => OperationTermination::Failed,
        EffectTermination::Partial => OperationTermination::Partial,
        EffectTermination::EffectUnknown => OperationTermination::EffectUnknown,
    };
    let execution = OperationReceipt {
        started_at: request.observed_at,
        ended_at: ended_at.max(request.observed_at),
        effective_deadline: request.context.deadline().clone(),
        cancellation: control_observation,
        budget: OperationBudgetUsage::default(),
        termination: operation_termination,
    };
    let receipt = EffectReceipt {
        operation: attempt.operation.use_case_id().clone(),
        request_id: request.context.request_id().clone(),
        actor: request.context.actor().clone(),
        scope: request.context.scope().clone(),
        effect_class: tracedecay_tool_catalog::EffectClass::SourceEdit,
        idempotency_key: request.attempt_idempotency_key.clone(),
        input_digest: attempt.input_digest.clone(),
        expected_state: journal.expected_state.clone(),
        policy_digest: attempt.authority.proof.policy_digest.clone(),
        configuration_digest: attempt.authority.proof.configuration_digest.clone(),
        catalog_digest: attempt.authority.proof.catalog_digest.clone(),
        privacy_digest: attempt.authority.proof.privacy_digest.clone(),
        outcome: termination,
        committed_state,
        external_proof: attempt.authority.proof.external_proof.clone(),
    };
    let effect_id =
        reconciliation_attempt_effect_id(&request.attempt_idempotency_key, attempt.input_digest)?;
    let durable_outcome =
        SourceEditDurableOutcomeV1::from_live(attempt.operation.use_case_id(), outcome);
    let effect = EffectResult::new(
        effect_id,
        tracedecay_tool_catalog::EffectClass::SourceEdit,
        request.attempt_idempotency_key.clone(),
        attempt.authority.receipt.clone(),
        journal.expected_state.clone(),
        execution,
        ReconciliationState::Reconciled,
        receipt,
        Some(durable_outcome.value()),
    )
    .map_err(application_contract_error)?;
    Ok(SourceEditDurableResultV1 {
        version: JOURNAL_VERSION,
        input_digest: attempt.input_digest.clone(),
        authority_proof: attempt.authority.proof.clone(),
        dry_run: false,
        predicted_state: journal.predicted_state.clone(),
        outcome: durable_outcome,
        effect,
    })
}

pub(super) fn durable_record(
    journal: &SourceEditJournalV1,
    outcome: SourceEditDurableOutcomeV1,
    committed_state: Option<ManifestDigest>,
    ended_at: UtcMicros,
    termination: EffectTermination,
    reconciliation: ReconciliationState,
    control_observation: Option<CancellationObservation>,
) -> Result<SourceEditDurableResultV1> {
    let request = &journal.request;
    let operation_termination = match termination {
        EffectTermination::Completed => OperationTermination::Completed,
        EffectTermination::Cancelled => OperationTermination::Cancelled,
        EffectTermination::TimedOut => OperationTermination::TimedOut,
        EffectTermination::Failed => OperationTermination::Failed,
        EffectTermination::Partial => OperationTermination::Partial,
        EffectTermination::EffectUnknown => OperationTermination::EffectUnknown,
    };
    let execution = OperationReceipt {
        started_at: request.started_at,
        ended_at: ended_at.max(request.started_at),
        effective_deadline: request.deadline.clone(),
        cancellation: control_observation,
        budget: OperationBudgetUsage::default(),
        termination: operation_termination,
    };
    let receipt = EffectReceipt {
        operation: request.operation.clone(),
        request_id: request.request_id.clone(),
        actor: request.actor.clone(),
        scope: request.scope.clone(),
        effect_class: tracedecay_tool_catalog::EffectClass::SourceEdit,
        idempotency_key: request.idempotency_key.clone(),
        input_digest: journal.input_digest.clone(),
        expected_state: journal.expected_state.clone(),
        policy_digest: request.authority_proof.policy_digest.clone(),
        configuration_digest: request.authority_proof.configuration_digest.clone(),
        catalog_digest: request.authority_proof.catalog_digest.clone(),
        privacy_digest: request.authority_proof.privacy_digest.clone(),
        outcome: termination,
        committed_state,
        external_proof: request.authority_proof.external_proof.clone(),
    };
    let effect = EffectResult::new(
        journal.effect_id.clone(),
        tracedecay_tool_catalog::EffectClass::SourceEdit,
        request.idempotency_key.clone(),
        request.authority.clone(),
        journal.expected_state.clone(),
        execution,
        reconciliation,
        receipt,
        Some(outcome.value()),
    )
    .map_err(application_contract_error)?;
    Ok(SourceEditDurableResultV1 {
        version: JOURNAL_VERSION,
        input_digest: journal.input_digest.clone(),
        authority_proof: request.authority_proof.clone(),
        dry_run: request.dry_run,
        predicted_state: journal.predicted_state.clone(),
        outcome,
        effect,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::test_support::*;

    use crate::edit::digest::persist_record;
    use std::fs;
    use tempfile::tempdir;
    use tracedecay_application::source_edit::MoveHint;
    use tracedecay_application::source_edit::{
        AstGrepResult, EditResult, InsertResult, MoveResult, MultiEditResult,
    };
    use tracedecay_application::{
        CancellationStage, OperationTermination, SourceEditDiagnosticV1, SourceEditRequest,
        SourceEditVerificationV1, source_edit_operation,
    };
    use tracedecay_domain::UtcMicros;

    #[test]
    fn committed_record_keeps_after_commit_cancellation_without_downgrade() {
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
        let observation = tracedecay_application::CancellationObservation {
            stage: CancellationStage::AfterCommit,
            observed_at: UtcMicros(5),
        };

        let record = applied_record(
            &journal,
            &outcome,
            digest(SHA256_B),
            UtcMicros(5),
            Some(observation.clone()),
        )
        .unwrap();

        assert_eq!(record.effect.receipt.outcome, EffectTermination::Completed);
        assert_eq!(
            record.effect.execution.termination,
            OperationTermination::Completed
        );
        assert_eq!(record.effect.execution.cancellation, Some(observation));
    }

    #[test]
    fn requested_incomplete_verification_makes_committed_effect_partial() {
        let request = fixture_request();
        let outcome = SourceEditOutcome::Edit(EditResult {
            success: true,
            file_path: "src/lib.rs".to_owned(),
            message: "applied".to_owned(),
            ..EditResult::default()
        });
        let mut journal = fixture_journal(
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
                verification_state: Some(SourceEditVerificationStateV1::Failed),
            },
        );
        journal.request.verification_requested = true;

        let record =
            applied_record(&journal, &outcome, digest(SHA256_B), UtcMicros(4), None).unwrap();

        assert_eq!(record.effect.receipt.outcome, EffectTermination::Partial);
        assert_eq!(
            record.effect.execution.termination,
            OperationTermination::Partial
        );
    }

    #[test]
    fn durable_receipt_rejects_unknown_version() {
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
        let journal = fixture_journal(&request, SourceEditJournalStateV1::Prepared);
        let mut record =
            applied_record(&journal, &outcome, digest(SHA256_B), UtcMicros(4), None).unwrap();
        record.version = JOURNAL_VERSION + 1;
        persist_record(
            &durability.receipt_path(&request.idempotency_key).unwrap(),
            "source-edit-receipt",
            &record,
        )
        .unwrap();

        assert!(durability.load_receipt(&request.idempotency_key).is_err());
    }

    #[test]
    fn durable_journal_and_receipt_never_retain_edit_bodies() {
        const SENTINEL: &str = "SOURCE_EDIT_BODY_MUST_NOT_PERSIST_7b6398";

        let directory = tempdir().unwrap();
        let durability = SourceEditDurability {
            root: directory.path().to_path_buf(),
        };
        let mut request = fixture_request();
        request.edit = SourceEditRequest::StrReplace {
            path: "src/lib.rs".to_owned(),
            old_str: SENTINEL.to_owned(),
            new_str: SENTINEL.to_owned(),
            dry_run: false,
            verify: true,
        };
        let outcomes = vec![
            SourceEditOutcome::Edit(EditResult {
                success: true,
                file_path: "src/lib.rs".to_owned(),
                matched_str: SENTINEL.to_owned(),
                new_str: SENTINEL.to_owned(),
                replaced_span: Some(SENTINEL.to_owned()),
                diff: Some(SENTINEL.to_owned()),
                message: SENTINEL.to_owned(),
                ..EditResult::default()
            }),
            SourceEditOutcome::MultiEdit(MultiEditResult {
                success: true,
                file_path: "src/lib.rs".to_owned(),
                applied_count: 2,
                diff: Some(SENTINEL.to_owned()),
                message: SENTINEL.to_owned(),
                ..MultiEditResult::default()
            }),
            SourceEditOutcome::Insert(InsertResult {
                success: true,
                file_path: "src/lib.rs".to_owned(),
                anchor_line: 7,
                content: SENTINEL.to_owned(),
                diff: Some(SENTINEL.to_owned()),
                message: SENTINEL.to_owned(),
                ..InsertResult::default()
            }),
            SourceEditOutcome::AstGrep(AstGrepResult {
                success: true,
                file_path: "src/lib.rs".to_owned(),
                pattern: SENTINEL.to_owned(),
                rewrite: SENTINEL.to_owned(),
                diff: Some(SENTINEL.to_owned()),
                message: SENTINEL.to_owned(),
                ..AstGrepResult::default()
            }),
            SourceEditOutcome::Move(MoveResult {
                success: true,
                symbol: "fixture_symbol".to_owned(),
                source_file: "src/lib.rs".to_owned(),
                dest_file: "src/moved.rs".to_owned(),
                moved_span: Some(SENTINEL.to_owned()),
                diff: Some(SENTINEL.to_owned()),
                applied_imports: vec![SENTINEL.to_owned()],
                impact: vec![MoveHint {
                    kind: "dependency_broken".to_owned(),
                    file: "src/lib.rs".to_owned(),
                    line: Some(7),
                    detail: SENTINEL.to_owned(),
                    suggestion: Some(SENTINEL.to_owned()),
                }],
                message: SENTINEL.to_owned(),
                ..MoveResult::default()
            }),
        ];
        let verification = SourceEditVerificationV1 {
            state: SourceEditVerificationStateV1::Errors,
            verdict: "errors".to_owned(),
            error_count: 1,
            warning_count: 0,
            first_errors: vec![SourceEditDiagnosticV1 {
                line: 7,
                code: "fixture".to_owned(),
                message: SENTINEL.to_owned(),
            }],
            message: None,
        };
        let operation = source_edit_operation(request.edit.kind()).unwrap();

        for outcome in outcomes {
            let journal = fixture_journal(
                &request,
                SourceEditJournalStateV1::Applied {
                    outcome: SourceEditDurableOutcomeV1::from_live(
                        operation.use_case_id(),
                        &outcome,
                    ),
                    committed_state: digest(SHA256_B),
                    ended_at: UtcMicros(4),
                    control_observation: None,
                    verification_state: None,
                },
            );
            durability.persist_journal(&journal).unwrap();
            let journal_json = fs::read_to_string(durability.journal_path()).unwrap();
            assert!(!journal_json.contains(SENTINEL));

            let record =
                applied_record(&journal, &outcome, digest(SHA256_B), UtcMicros(4), None).unwrap();
            let live = record
                .clone()
                .into_live_application_result(outcome, Some(verification.clone()));
            assert!(live.value().to_string().contains(SENTINEL));

            durability.persist_receipt(&record).unwrap();
            let receipt_json =
                fs::read_to_string(durability.receipt_path(&request.idempotency_key).unwrap())
                    .unwrap();
            assert!(!receipt_json.contains(SENTINEL));
            for forbidden_key in [
                "matched_str",
                "new_str",
                "content",
                "pattern",
                "rewrite",
                "replaced_span",
                "moved_span",
                "diff",
                "applied_imports",
                "impact",
                "detail",
                "suggestion",
                "verification",
            ] {
                assert!(!journal_json.contains(&format!("\"{forbidden_key}\"")));
                assert!(!receipt_json.contains(&format!("\"{forbidden_key}\"")));
            }

            let replay = durability
                .load_receipt(&request.idempotency_key)
                .unwrap()
                .unwrap()
                .into_application_result(true);
            let replay_value = replay.value();
            assert_eq!(replay_value["durable_metadata_only"], true);
            assert!(!replay_value.to_string().contains(SENTINEL));
        }
    }
}
