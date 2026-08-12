use super::*;

use serde_json::json;
use tracedecay_application::retained_surfaces::{
    AutomationCommittedReceiptV1, AutomationRunProblemV1, AutomationRunRequestV1,
    AutomationRunResultV1, AutomationRunSummaryV1, AutomationRunTerminalV1,
    AutomationTaskRequestV1, AutomationTaskV1, MemoryAutomationCurationReceiptV1,
    MemoryCuratorRunInputV1, RetainedSurfaceExecutionErrorV1, RetainedSurfaceOperation,
    RetainedSurfaceResultV1, UserJobRunInputV1, retained_surface_application_operation,
    retained_surface_execution_problem,
};
use tracedecay_application::{
    ApplicationOutcome, ApplicationProblemEnvelope, AuthorityReceipt, Deadline, DisclosureClass,
    EffectId, EffectReceipt, EffectResult, EffectTermination, IdempotencyKey, OperationReceipt,
    PolicyDecisionRef, ReconciliationState, RequestId, ResolvedScope,
};
use tracedecay_domain::{
    ActorId, ComponentVersion, ManifestDigest, ProjectId, RepositoryId, RunId, UtcMicros,
    WorktreeId, canonical_sha256,
};
use tracedecay_tool_catalog::EffectClass;

use crate::daemon::automation_effect::{AutomationSettledTerminal, recovery_index};

fn digest(seed: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", seed.to_string().repeat(64))).expect("fixture digest")
}

fn scope() -> ResolvedScope {
    ResolvedScope::new(
        ProjectId::new("project.memory-journal").expect("project"),
        RepositoryId::new("repository.memory-journal").expect("repository"),
        WorktreeId::new("worktree.memory-journal").expect("worktree"),
        None,
    )
    .expect("scope")
}

fn request(run_id: &str) -> AutomationRunRequestV1 {
    AutomationRunRequestV1 {
        run_id: RunId::new(run_id).expect("run id"),
        task: AutomationTaskRequestV1::MemoryCurator(MemoryCuratorRunInputV1 {
            fact_review_limit: 24,
            min_confidence_millionths: 720_000,
        }),
    }
}

fn reset_problem(
    request_id: &RequestId,
    scope: &ResolvedScope,
    request: &AutomationRunRequestV1,
) -> AutomationRunProblemV1 {
    let operation = retained_surface_application_operation(RetainedSurfaceOperation::AutomationRun)
        .expect("operation");
    let problem =
        retained_surface_execution_problem(RetainedSurfaceExecutionErrorV1::ProjectResetRequired);
    let problem = ApplicationProblemEnvelope::new(
        operation.result_contract().clone(),
        request_id.clone(),
        problem,
    )
    .expect("problem envelope");
    AutomationRunProblemV1::new(request, scope.clone(), problem, Vec::new(), request_id)
        .expect("reset terminal")
}

fn admission(run_id: &str, request_id: &str) -> DurableAutomationAdmission {
    let request_id = RequestId::new(request_id).expect("request id");
    let scope = scope();
    let request = request(run_id);
    DurableAutomationAdmission {
        schema_version: 1,
        request: request.clone(),
        input_digest: digest('1'),
        configuration_digest: digest('2'),
        effect_authority_digest: digest('a'),
        grant_id: tracedecay_application::CapabilityGrantId::new("grant.memory-journal")
            .expect("grant"),
        grant_revision: 1,
        grant_digest: digest('6'),
        disclosure: DisclosureClass::Evidence,
        effect_receipt_template: partial_receipt_template(&request_id, &scope),
        actor: ActorId::new("actor.memory-journal").expect("actor"),
        scope: scope.clone(),
        request_id: request_id.clone(),
        process_run_id: "process.memory-journal".to_owned(),
        recovery: AutomationRecoveryBinding::Memory {
            owner: FactOwnerV1::Project {
                project_id: scope.project_id.clone(),
            },
            recovery_problem: reset_problem(&request_id, &scope, &request),
            retirement: None,
            reset_source_digest: None,
        },
    }
}

fn external_admission(run_id: &str, request_id: &str) -> DurableAutomationAdmission {
    let mut admission = admission(run_id, request_id);
    let request = AutomationRunRequestV1 {
        run_id: admission.request.run_id.clone(),
        task: AutomationTaskRequestV1::UserJob(UserJobRunInputV1 {
            job_id: "nightly-delivery".to_owned(),
        }),
    };
    admission.recovery = AutomationRecoveryBinding::External {
        recovery_problem: reset_problem(&admission.request_id, &admission.scope, &request),
    };
    admission.request = request;
    admission
}

fn partial_receipt_template(request_id: &RequestId, scope: &ResolvedScope) -> EffectReceipt {
    let operation = retained_surface_application_operation(RetainedSurfaceOperation::AutomationRun)
        .expect("operation");
    EffectReceipt {
        operation: operation.use_case_id().clone(),
        request_id: request_id.clone(),
        actor: ActorId::new("actor.memory-journal").expect("actor"),
        scope: scope.clone(),
        effect_class: EffectClass::Administrative,
        idempotency_key: IdempotencyKey::new("idempotency.memory-journal").expect("key"),
        input_digest: digest('1'),
        expected_state: digest('5'),
        policy_digest: digest('6'),
        configuration_digest: digest('2'),
        catalog_digest: digest('7'),
        privacy_digest: digest('8'),
        outcome: EffectTermination::Partial,
        committed_state: None,
        external_proof: None,
    }
}

fn authority(scope: &ResolvedScope) -> AuthorityReceipt {
    AuthorityReceipt {
        grant_id: tracedecay_application::CapabilityGrantId::new("grant.memory-journal")
            .expect("grant"),
        grant_revision: 1,
        grant_digest: digest('6'),
        authorized_scope_digest: scope.scope_digest.clone(),
        disclosure: DisclosureClass::Evidence,
        policy: PolicyDecisionRef::new(
            "policy.memory-journal",
            1,
            digest('6'),
            ComponentVersion::new("policy.memory-journal.v1").expect("component"),
        )
        .expect("policy"),
        revalidated_at: UtcMicros(2),
    }
}

fn success_terminal(
    admission: &DurableAutomationAdmission,
    result_run_id: &str,
) -> AutomationSettledTerminal {
    let operation = retained_surface_application_operation(RetainedSurfaceOperation::AutomationRun)
        .expect("operation");
    let expected_state = digest('5');
    let idempotency_key = IdempotencyKey::new("idempotency.memory-journal").expect("key");
    let receipt = EffectReceipt {
        operation: operation.use_case_id().clone(),
        request_id: admission.request_id.clone(),
        actor: admission.actor.clone(),
        scope: admission.scope.clone(),
        effect_class: EffectClass::Administrative,
        idempotency_key: idempotency_key.clone(),
        input_digest: admission.input_digest.clone(),
        expected_state: expected_state.clone(),
        policy_digest: digest('6'),
        configuration_digest: admission.configuration_digest.clone(),
        catalog_digest: digest('7'),
        privacy_digest: digest('8'),
        outcome: EffectTermination::Completed,
        committed_state: Some(digest('9')),
        external_proof: None,
    };
    let result = AutomationRunResultV1 {
        run_id: RunId::new(result_run_id).expect("result run id"),
        task: AutomationTaskV1::MemoryCurator,
        request_digest: admission.request.input_digest().expect("request digest"),
        terminal: AutomationRunTerminalV1::Completed {
            summary: AutomationRunSummaryV1 {
                reviewed_count: 0,
                accepted_count: 0,
                rejected_count: 0,
                skipped_count: 0,
            },
        },
        committed_receipts: Vec::new(),
    };
    let effect = EffectResult::new(
        EffectId::new("effect.memory-journal").expect("effect"),
        EffectClass::Administrative,
        idempotency_key,
        authority(&admission.scope),
        expected_state,
        OperationReceipt::completed(
            UtcMicros(1),
            UtcMicros(2),
            Deadline::new(UtcMicros(10)).expect("deadline"),
            Default::default(),
        )
        .expect("execution"),
        ReconciliationState::Reconciled,
        receipt,
        Some(RetainedSurfaceResultV1::AutomationRun(result)),
    )
    .expect("effect result");
    AutomationSettledTerminal::Outcome {
        scope: admission.scope.clone(),
        outcome: ApplicationOutcome::Effect(effect),
    }
}

fn partial_terminal(admission: &DurableAutomationAdmission) -> AutomationSettledTerminal {
    let receipt_run_id = admission.request.run_id.as_str();
    let mut receipt: MemoryAutomationCurationReceiptV1 = serde_json::from_value(json!({
            "receipt": {
                "owner": {"kind":"project","project_id":"project.memory-journal"},
                "operation_id":"operation.memory-journal",
                "input_digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "automation_run_id": receipt_run_id,
                "operation_effects":[{"kind":"normalize_tags","fact_id":"fact.memory-journal","commit":{"disposition":"committed","fact_id":"fact.memory-journal","owner":{"kind":"project","project_id":"project.memory-journal"},"committed_event_ids":["event.memory-journal.fact","event.memory-journal.provenance"],"last_event_id":"event.memory-journal.provenance","active_assertion_id":"assertion.memory-journal"}}],
                "replay_fact_id":"fact.memory-journal","replay_event_id":"event.memory-journal.provenance","changed_fact_ids":["fact.memory-journal"],"normalized_tags":1,"facts_linked":0
            },
            "canonical_digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        }))
        .expect("curation receipt");
    receipt.canonical_digest = receipt.canonical_digest().expect("canonical digest");
    let committed_receipts = vec![AutomationCommittedReceiptV1::Curation(receipt)];
    let committed_state = canonical_sha256(&(
        "tracedecay.automation-run.partial-state.v1",
        receipt_run_id,
        &committed_receipts,
    ))
    .expect("committed state");
    let operation = retained_surface_application_operation(RetainedSurfaceOperation::AutomationRun)
        .expect("operation");
    let effect_receipt = EffectReceipt {
        operation: operation.use_case_id().clone(),
        request_id: admission.request_id.clone(),
        actor: admission.actor.clone(),
        scope: admission.scope.clone(),
        effect_class: EffectClass::Administrative,
        idempotency_key: IdempotencyKey::new("idempotency.memory-journal.partial").expect("key"),
        input_digest: admission.input_digest.clone(),
        expected_state: digest('5'),
        policy_digest: digest('6'),
        configuration_digest: admission.configuration_digest.clone(),
        catalog_digest: digest('7'),
        privacy_digest: digest('8'),
        outcome: EffectTermination::Partial,
        committed_state: Some(committed_state),
        external_proof: None,
    };
    let problem =
        retained_surface_execution_problem(RetainedSurfaceExecutionErrorV1::PartialEffect {
            reason_code: "application.automation-run.partial-effect".to_owned(),
            committed_receipt: effect_receipt,
            detail: "canonical memory effect committed before delivery".to_owned(),
        });
    let problem = ApplicationProblemEnvelope::new(
        operation.result_contract().clone(),
        admission.request_id.clone(),
        problem,
    )
    .expect("problem envelope");
    AutomationSettledTerminal::Problem(
        AutomationRunProblemV1::new(
            &admission.request,
            admission.scope.clone(),
            problem,
            committed_receipts,
            &admission.request_id,
        )
        .expect("partial terminal"),
    )
}

#[test]
fn durable_journal_reopens_the_byte_identical_terminal() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("terminal.json");
    let admission = admission("run.memory-journal", "request.memory-journal");
    assert!(matches!(
        reserve_or_replay_blocking(&path, admission.clone()).expect("reserve"),
        ReservationResult::Execute { .. }
    ));
    let terminal = success_terminal(&admission, "run.memory-journal");
    let stored =
        persist_terminal_blocking(&path, &admission, terminal.clone()).expect("persist terminal");
    assert_eq!(stored, terminal);
    let replay = reserve_or_replay_blocking(&path, admission).expect("physical reopen");
    let ReservationResult::Replay {
        terminal: replay, ..
    } = replay
    else {
        panic!("terminal must replay")
    };
    assert_eq!(replay, terminal);
}

#[test]
fn foreign_external_reservation_closes_indeterminate_without_a_second_execution() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("external-terminal.json");
    let original = external_admission("run.external-journal", "request.external-journal");
    assert!(matches!(
        reserve_or_replay_blocking(&path, original.clone()).expect("reserve external run"),
        ReservationResult::Execute { .. }
    ));

    let mut reopened = original.clone();
    reopened.process_run_id = "process.external-journal.reopened".to_owned();
    let ReservationResult::Recover { .. } =
        reserve_or_replay_blocking(&path, reopened.clone()).expect("recover external reservation")
    else {
        panic!("foreign external reservation must recover")
    };
    assert!(reopened.is_external());
    let terminal = AutomationSettledTerminal::Problem(reopened.recovery_problem().clone());
    let stored = persist_recovered_terminal_blocking(&path, &reopened, terminal.clone(), None)
        .expect("close external reservation")
        .expect("recovery was not cancelled");
    assert_eq!(stored, terminal);

    let mut replay_request = original;
    replay_request.process_run_id = "process.external-journal.third".to_owned();
    let ReservationResult::Replay {
        terminal: replayed, ..
    } = reserve_or_replay_blocking(&path, replay_request).expect("replay indeterminate terminal")
    else {
        panic!("closed external reservation must replay its exact problem")
    };
    assert_eq!(replayed, terminal);
}

#[test]
fn durable_journal_rejects_changed_request_identity() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("terminal.json");
    reserve_or_replay_blocking(
        &path,
        admission("run.memory-journal", "request.memory-journal"),
    )
    .expect("reserve");
    let changed = admission("run.memory-journal", "request.memory-journal.changed");
    assert!(reserve_or_replay_blocking(&path, changed).is_err());
}

#[test]
fn durable_journal_rejects_changed_task_identity() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("terminal.json");
    let original = admission("run.memory-journal", "request.memory-journal");
    reserve_or_replay_blocking(&path, original).expect("reserve");
    let mut changed = admission("run.memory-journal", "request.memory-journal");
    changed.request.task = AutomationTaskRequestV1::SessionReflector(
        tracedecay_application::retained_surfaces::SessionReflectorRunInputV1 {
            provider: "cursor".to_owned(),
            query: "changed task".to_owned(),
            scope: tracedecay_application::retained_surfaces::LcmSearchScopeV1::Current,
            session_id: None,
            include_summaries: true,
            evidence_limit: 5,
            include_recent_sessions: false,
            recent_sessions_limit: 1,
            sort: tracedecay_application::retained_surfaces::LcmGrepSortV1::Recency,
            source: None,
            role: None,
            start_time: None,
            end_time: None,
        },
    );
    assert!(reserve_or_replay_blocking(&path, changed).is_err());
}

#[test]
fn durable_journal_rejects_changed_scope_identity() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("terminal.json");
    let original = admission("run.memory-journal", "request.memory-journal");
    reserve_or_replay_blocking(&path, original).expect("reserve");
    let mut changed = admission("run.memory-journal", "request.memory-journal");
    changed.scope = ResolvedScope::new(
        ProjectId::new("project.memory-journal.other").expect("project"),
        RepositoryId::new("repository.memory-journal").expect("repository"),
        WorktreeId::new("worktree.memory-journal").expect("worktree"),
        None,
    )
    .expect("scope");
    assert!(reserve_or_replay_blocking(&path, changed).is_err());
}

#[test]
fn durable_journal_rejects_changed_effect_authority() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("terminal.json");
    let original = admission("run.memory-journal", "request.memory-journal");
    reserve_or_replay_blocking(&path, original.clone()).expect("reserve");
    let mut changed = original;
    changed.effect_authority_digest = digest('b');
    assert!(reserve_or_replay_blocking(&path, changed).is_err());
}

#[test]
fn durable_journal_rejects_changed_project_owner() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("terminal.json");
    let original = admission("run.memory-journal", "request.memory-journal");
    reserve_or_replay_blocking(&path, original).expect("reserve");
    let mut changed = admission("run.memory-journal", "request.memory-journal");
    let AutomationRecoveryBinding::Memory { owner, .. } = &mut changed.recovery else {
        panic!("memory admission must carry memory recovery")
    };
    *owner = FactOwnerV1::Project {
        project_id: ProjectId::new("project.memory-journal.other").expect("project"),
    };
    assert!(reserve_or_replay_blocking(&path, changed).is_err());
}

#[test]
fn physical_reopen_retains_original_grant_when_current_registration_rotates() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("terminal.json");
    let original = admission("run.memory-journal", "request.memory-journal");
    reserve_or_replay_blocking(&path, original.clone()).expect("reserve");
    let rotated_grant =
        tracedecay_application::CapabilityGrantId::new("grant.rotated").expect("rotated grant");
    let reopened = read_indexed_record_blocking(&path)
        .expect("physical reopen")
        .expect("record");
    assert_eq!(reopened.admission().grant_id, original.grant_id);
    assert_ne!(reopened.admission().grant_id, rotated_grant);
    assert_eq!(
        reopened.admission().effect_authority_digest,
        original.effect_authority_digest
    );
}

#[test]
fn project_open_crash_recovery_defers_retirement_until_exact_finalization() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("terminal.json");
    let mut original = admission("run.memory-retirement", "request.memory-retirement");
    let binding = super::super::retirement::RetirementBinding {
        source_digest: format!("sha256:{}", "a".repeat(64)),
        archive_name: format!("fact_proposals.{}.json", "a".repeat(64)),
    };
    let AutomationRecoveryBinding::Memory { retirement, .. } = &mut original.recovery else {
        panic!("memory admission must carry memory recovery")
    };
    *retirement = Some(binding.clone());
    reserve_or_replay_blocking(&path, original.clone()).expect("reserve retirement");
    let mut reopened = original.clone();
    reopened.process_run_id = "process.memory-journal.reopened".to_owned();
    let ReservationResult::Recover { retirement } =
        reserve_or_replay_blocking(&path, reopened.clone()).expect("recover crashed reservation")
    else {
        panic!("crashed retirement must require canonical receipt recovery")
    };
    assert_eq!(retirement, Some(binding.clone()));
    let reopened_record = read_indexed_record_blocking(&path)
        .expect("physical reopen")
        .expect("reserved retirement");
    assert_eq!(reopened_record.admission().retirement(), Some(&binding));
    assert_eq!(
        super::super::recovery_index::special_recovery_defer_reason(
            reopened_record.admission(),
            true,
        ),
        Some("retirement_requires_exact_finalization")
    );
    assert_eq!(
        super::super::recovery_index::special_recovery_defer_reason(
            reopened_record.admission(),
            false,
        ),
        None
    );
    assert!(reopened_record.terminal().is_none());
    assert!(matches!(
        reserve_or_replay_blocking(&path, reopened)
            .expect("deferred retirement remains recoverable"),
        ReservationResult::Recover { .. }
    ));
}

#[test]
fn project_open_crash_recovery_preserves_shipped_reset_digest_until_exact_diagnostic() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("terminal.json");
    let mut original = admission("run.memory-reset", "request.memory-reset");
    let reset_digest = format!("sha256:{}", "b".repeat(64));
    let AutomationRecoveryBinding::Memory {
        reset_source_digest,
        ..
    } = &mut original.recovery
    else {
        panic!("memory admission must carry memory recovery")
    };
    *reset_source_digest = Some(reset_digest.clone());
    reserve_or_replay_blocking(&path, original.clone()).expect("reserve shipped reset");
    let mut reopened = original.clone();
    reopened.process_run_id = "process.memory-journal.reopened".to_owned();
    assert!(matches!(
        reserve_or_replay_blocking(&path, reopened.clone()).expect("recover crashed reset"),
        ReservationResult::Recover { .. }
    ));
    let reopened_record = read_indexed_record_blocking(&path)
        .expect("physical reopen")
        .expect("reserved shipped reset");
    assert_eq!(
        reopened_record.admission().reset_source_digest(),
        Some(reset_digest.as_str())
    );
    assert_eq!(
        super::super::recovery_index::special_recovery_defer_reason(
            reopened_record.admission(),
            true,
        ),
        Some("shipped_proposals_require_exact_reset_diagnostic")
    );
    assert!(reopened_record.terminal().is_none());
    assert!(matches!(
        reserve_or_replay_blocking(&path, reopened).expect("deferred reset remains recoverable"),
        ReservationResult::Recover { .. }
    ));
}

#[test]
fn foreign_reservation_recovery_persists_exact_partial_terminal() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("terminal.json");
    let original = admission("run.memory-journal", "request.memory-journal");
    reserve_or_replay_blocking(&path, original.clone()).expect("reserve");
    let mut reopened = original.clone();
    reopened.process_run_id = "process.memory-journal.reopened".to_owned();
    assert!(matches!(
        reserve_or_replay_blocking(&path, reopened.clone()).expect("recover"),
        ReservationResult::Recover { .. }
    ));
    let partial = partial_terminal(&original);
    let stored = persist_recovered_terminal_blocking(&path, &reopened, partial.clone(), None)
        .expect("persist recovered partial terminal");
    let stored = stored.expect("active recovery");
    assert_eq!(
        serde_json::to_vec(&stored).expect("stored bytes"),
        serde_json::to_vec(&partial).expect("partial bytes")
    );
    let ReservationResult::Replay {
        terminal: replay, ..
    } = reserve_or_replay_blocking(&path, reopened).expect("physical reopen")
    else {
        panic!("recovered partial terminal must replay")
    };
    assert_eq!(
        serde_json::to_vec(&replay).expect("replay bytes"),
        serde_json::to_vec(&partial).expect("partial bytes")
    );
}

#[test]
fn uncommitted_combined_fallback_rolls_back_its_reservation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("terminal.json");
    let admission = admission("run.memory-journal", "request.memory-journal");
    reserve_or_replay_blocking(&path, admission.clone()).expect("reserve");
    abandon_reservation_blocking(&path, &admission).expect("rollback reservation");
    assert!(!path.exists());
    assert!(matches!(
        reserve_or_replay_blocking(&path, admission).expect("fresh reserve"),
        ReservationResult::Execute { .. }
    ));
}

#[test]
fn durable_journal_rejects_a_swapped_success_run_before_write() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("terminal.json");
    let admission = admission("run.memory-journal", "request.memory-journal");
    reserve_or_replay_blocking(&path, admission.clone()).expect("reserve");
    let swapped = success_terminal(&admission, "run.memory-journal.other");
    assert!(persist_terminal_blocking(&path, &admission, swapped).is_err());
    assert!(matches!(
        read_record(&path)
            .expect("read reservation")
            .expect("record")
            .state,
        DurableAutomationState::Reserved
    ));
}

#[test]
fn durable_journal_rejects_swapped_partial_receipts_before_write() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("terminal.json");
    let admission = admission("run.memory-journal", "request.memory-journal");
    reserve_or_replay_blocking(&path, admission.clone()).expect("reserve");
    let mut swapped = partial_terminal(&admission);
    let AutomationSettledTerminal::Problem(problem) = &mut swapped else {
        panic!("partial terminal must be a problem")
    };
    let AutomationCommittedReceiptV1::Curation(receipt) = &mut problem.committed_receipts[0] else {
        panic!("partial fixture must carry a curation receipt")
    };
    receipt.receipt.automation_run_id =
        RunId::new("run.memory-journal.other").expect("other run id");
    receipt.canonical_digest = receipt.canonical_digest().expect("canonical digest");
    let committed_state = canonical_sha256(&(
        "tracedecay.automation-run.partial-state.v1",
        problem.run_id.as_str(),
        &problem.committed_receipts,
    ))
    .expect("committed state");
    problem
        .problem
        .problem
        .committed_receipt
        .as_mut()
        .expect("partial effect receipt")
        .committed_state = Some(committed_state);
    assert!(persist_terminal_blocking(&path, &admission, swapped).is_err());
    assert!(matches!(
        read_record(&path)
            .expect("read reservation")
            .expect("record")
            .state,
        DurableAutomationState::Reserved
    ));
}

#[test]
fn physical_reopen_rejects_a_corrupt_swapped_terminal() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("terminal.json");
    let admission = admission("run.memory-journal", "request.memory-journal");
    write_record(
        &path,
        &DurableAutomationRecord {
            admission: admission.clone(),
            state: DurableAutomationState::Terminal(success_terminal(
                &admission,
                "run.memory-journal.other",
            )),
        },
    )
    .expect("write corrupt fixture");
    assert!(reserve_or_replay_blocking(&path, admission).is_err());
}

#[test]
fn scheduler_stable_request_identity_reopens_the_same_terminal() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("terminal.json");
    let first_request =
        crate::daemon::scheduler::effect_admission::scheduler_automation_request_id(Some(
            "host_receipt_17",
        ))
        .expect("first scheduler identity");
    let reopened_request =
        crate::daemon::scheduler::effect_admission::scheduler_automation_request_id(Some(
            "host_receipt_17",
        ))
        .expect("reopened scheduler identity");
    assert_eq!(first_request, reopened_request);
    let durable_admission = admission("host_receipt_17", first_request.as_str());
    reserve_or_replay_blocking(&path, durable_admission.clone()).expect("reserve");
    let terminal = success_terminal(&durable_admission, "host_receipt_17");
    persist_terminal_blocking(&path, &durable_admission, terminal.clone()).expect("persist");
    let reopened = admission("host_receipt_17", reopened_request.as_str());
    let ReservationResult::Replay {
        terminal: replay, ..
    } = reserve_or_replay_blocking(&path, reopened).expect("scheduler physical reopen")
    else {
        panic!("scheduler terminal must replay")
    };
    assert_eq!(replay, terminal);
}

#[test]
fn pending_index_survives_physical_reopen_and_closes_after_terminal() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dashboard_root = temp.path();
    let path = dashboard_root
        .join("automation_effects")
        .join(format!("{}.json", "a".repeat(64)));
    let admission = admission("indexed_physical_reopen", "request.indexed-reopen");
    recovery_index::add_pending_blocking(dashboard_root, &path, &admission)
        .expect("durable pending index");
    reserve_or_replay_blocking(&path, admission.clone()).expect("reserve");

    let reopened = recovery_index::indexed_journals_blocking(dashboard_root, &admission.scope)
        .expect("physical index reopen");
    assert_eq!(reopened.len(), 1);
    assert_eq!(reopened[0].path, path);
    let record = read_indexed_record_blocking(&path)
        .expect("journal read")
        .expect("reserved journal");
    assert!(record.terminal().is_none());
    assert_eq!(record.admission(), &admission);

    let terminal = partial_terminal(&admission);
    let mut reopened_admission = admission.clone();
    reopened_admission.process_run_id = "process.reopened".to_owned();
    persist_recovered_terminal_blocking(&path, &reopened_admission, terminal, None)
        .expect("recovered terminal");
    recovery_index::remove_pending_blocking(dashboard_root, &path).expect("pending cleanup");
    assert!(
        recovery_index::indexed_journals_blocking(dashboard_root, &admission.scope,)
            .expect("closed index reopen")
            .is_empty()
    );
}

#[test]
fn cancellation_observed_under_lock_leaves_foreign_reservation_pending() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("terminal.json");
    let original = admission("cancelled_recovery", "request.cancelled-recovery");
    reserve_or_replay_blocking(&path, original.clone()).expect("reserve");
    let mut reopened = original.clone();
    reopened.process_run_id = "process.reopened".to_owned();
    let cancellation =
        tracedecay_application::CancellationSignal::active("cancellation.memory-journal.recovery")
            .expect("cancellation");
    assert!(cancellation.cancel(UtcMicros(20)));
    assert!(
        persist_recovered_terminal_blocking(
            &path,
            &reopened,
            partial_terminal(&original),
            Some(&cancellation),
        )
        .expect("cancelled recovery")
        .is_none()
    );
    assert!(
        read_indexed_record_blocking(&path)
            .expect("read reservation")
            .expect("record")
            .terminal()
            .is_none()
    );
}
