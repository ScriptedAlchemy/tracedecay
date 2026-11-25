use tracedecay_automation_runtime::automation::effect_runtime::contract_error;
use tracedecay_automation_runtime::automation::effect_runtime::settlement::*;
use tracedecay_automation_runtime::automation::run_ledger::ExactRunPublication;

use tracedecay_contracts::retained_surfaces::{
    AutomationRunProblemV1, AutomationRunRequestV1, AutomationTaskRequestV1,
    MemoryCuratorRunInputV1, RetainedSurfaceExecutionErrorV1, RetainedSurfaceOperation,
    UserJobRunInputV1, retained_surface_application_operation, retained_surface_execution_problem,
};
#[cfg(unix)]
use tracedecay_contracts::retained_surfaces::{
    AutomationRunResultV1, AutomationRunSummaryV1, AutomationRunTerminalV1, AutomationTaskV1,
};

#[cfg(unix)]
use tracedecay_contracts::{
    ApplicationOutcome, AuthorityReceipt, EffectId, EffectResult, OperationBudgetUsage,
    OperationReceipt, PolicyDecisionRef, ReconciliationState,
};
use tracedecay_contracts::{
    ApplicationProblemEnvelope, Deadline, DisclosureClass, EffectReceipt, EffectTermination,
    IdempotencyKey, RequestId, ResolvedScope,
};
#[cfg(unix)]
use tracedecay_domain::ComponentVersion;
use tracedecay_domain::{
    ActorId, FactOwnerV1, ProjectId, RepositoryId, RunId, UtcMicros, WorktreeId, canonical_sha256,
};
use tracedecay_tool_catalog::EffectClass;

use crate::automation_effect::recovery_composition;

#[test]
fn legacy_run_publication_is_refused_without_repeating_its_external_effect() {
    for promoted in [false, true] {
        let directory = tempfile::tempdir().expect("legacy journal directory");
        let admission = external_admission_for_job(
            "legacy-publication",
            "request.legacy-publication",
            "legacy-job",
        );
        let journal_path = canonical_journal_path(directory.path(), &admission.request.run_id);
        let reservation = reserve_or_replay_blocking(&journal_path, admission.clone())
            .expect("reserve the original occurrence");
        let terminal = AutomationSettledTerminal::Problem(admission.recovery_problem().clone());
        let publication: ExactRunPublication = serde_json::from_value(serde_json::json!({
            "schema_version": 2,
            "digest": digest('d'),
        }))
        .expect("current publication");
        persist_prepared_terminal_blocking(
            &journal_path,
            &admission,
            &terminal,
            publication.clone(),
        )
        .expect("write the real prepared journal and sidecar");
        if promoted {
            promote_prepared_terminal_blocking(
                &journal_path,
                &admission,
                terminal,
                &publication,
            )
            .expect("write the real terminal journal");
        }
        drop(reservation);
        let mut document: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&journal_path).expect("read the real journal shape"),
        )
        .expect("journal JSON");
        *document
            .pointer_mut("/state/value/publication")
            .expect("publication inside the serialized journal state") = serde_json::json!({
            "schema_version": 1,
            "ledger_digest": digest('d'),
            "payload_len": 1,
        });
        let retired = serde_json::to_vec(&document).expect("retired journal bytes");
        std::fs::write(&journal_path, &retired).expect("write legacy publication");
        let sidecar_path = journal_path.with_extension("json.terminal");
        let sidecar = std::fs::read(&sidecar_path).expect("original terminal receipt");

        for _ in 0..2 {
            let error = reserve_or_replay_blocking(&journal_path, admission.clone())
                .err()
                .expect("the old publication must stop before another effect");
            assert_eq!(
                error
                    .project_route_context()
                    .map(|(code, retryable, _)| (code, retryable)),
                Some(("automation_run_legacy_jsonl_refused", false))
            );
            assert!(error.reset_required_context().is_none());
        }

        let next = external_admission_for_job(
            "new-occurrence",
            "request.new-occurrence",
            "legacy-job",
        );
        let next_path = canonical_journal_path(directory.path(), &next.request.run_id);
        assert!(matches!(
            reserve_or_replay_blocking(&next_path, next).expect("explicitly new occurrence"),
            ReservationResult::Execute { .. }
        ));
        assert_eq!(std::fs::read(&journal_path).expect("retained journal"), retired);
        assert_eq!(std::fs::read(&sidecar_path).expect("retained receipt"), sidecar);
    }
}
use tracedecay_automation_runtime::automation::effect_runtime::AutomationSettledTerminal;
use tracedecay_automation_runtime::automation::effect_runtime::journal::*;
use tracedecay_automation_runtime::automation::effect_runtime::recovery_index;
use tracedecay_automation_runtime::automation::registered_run_store::RegisteredAutomationRunStore;

struct NeverAutomationBackend;

impl tracedecay_automation_runtime::automation::backend::AgentTaskBackend
    for NeverAutomationBackend
{
    fn run_task(
        &self,
        _request: &tracedecay_automation_runtime::automation::backend::AgentTaskRequest,
    ) -> std::result::Result<
        tracedecay_automation_runtime::automation::backend::AgentTaskResponse,
        tracedecay_automation_runtime::automation::backend::AgentTaskError,
    > {
        panic!("disabled retained automation must not invoke its backend")
    }

    fn executable(&self) -> Option<&std::path::Path> {
        None
    }
}

use tracedecay_domain::test_fixtures::digest;

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
    let operation =
        retained_surface_application_operation(RetainedSurfaceOperation::FactStoreCurate)
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

fn seal_effect_authority(mut admission: DurableAutomationAdmission) -> DurableAutomationAdmission {
    let operation =
        retained_surface_application_operation(RetainedSurfaceOperation::FactStoreCurate)
            .expect("operation");
    admission.effect_authority_digest = recovery_index::effect_authority_digest(
        admission.schema_version,
        &operation,
        &admission.request,
        &admission.input_digest,
        &admission.configuration_digest,
        &admission.grant_id,
        admission.grant_revision,
        &admission.grant_digest,
        &admission.disclosure,
        &admission.effect_receipt_template,
        &admission.actor,
        &admission.scope,
        &admission.request_id,
        &admission.recovery,
    )
    .expect("effect authority digest");
    admission
}

fn admission(run_id: &str, request_id: &str) -> DurableAutomationAdmission {
    let request_id = RequestId::new(request_id).expect("request id");
    let scope = scope();
    let request = request(run_id);
    seal_effect_authority(DurableAutomationAdmission {
        schema_version: 1,
        request: request.clone(),
        input_digest: digest('0'),
        configuration_digest: digest('2'),
        effect_authority_digest: digest('a'),
        grant_id: tracedecay_contracts::CapabilityGrantId::new("grant.memory-journal")
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
        },
    })
}

fn external_admission_for_job(
    run_id: &str,
    request_id: &str,
    job_id: &str,
) -> DurableAutomationAdmission {
    let mut admission = admission(run_id, request_id);
    let request = AutomationRunRequestV1 {
        run_id: admission.request.run_id.clone(),
        task: AutomationTaskRequestV1::UserJob(UserJobRunInputV1 {
            job_id: job_id.to_owned(),
        }),
    };
    admission.recovery = AutomationRecoveryBinding::External {
        recovery_problem: reset_problem(&admission.request_id, &admission.scope, &request),
    };
    admission.request = request;
    seal_effect_authority(admission)
}

fn canonical_journal_path(dashboard_root: &std::path::Path, run_id: &RunId) -> std::path::PathBuf {
    let key = canonical_sha256(&("tracedecay.automation-run.terminal-key.v1", run_id))
        .expect("automation journal key");
    dashboard_root.join("automation_effects").join(format!(
        "{}.json",
        key.as_str().trim_start_matches("sha256:")
    ))
}

fn external_admission_for_recovery_project(
    cg: &tracedecay_project::project::TraceDecay,
    run_id: &str,
    request_id: &str,
    job_id: &str,
) -> DurableAutomationAdmission {
    let owner = cg.project_memory_owner().expect("project memory owner");
    let FactOwnerV1::Project { project_id } = owner else {
        panic!("automation recovery fixture requires a project owner")
    };
    let recovery_scope =
        tracedecay_code_index_runtime::resolved_scope_for_project(cg.project_root(), &project_id)
            .expect("recovery scope");
    let mut admission = external_admission_for_job(run_id, request_id, job_id);
    admission.scope = recovery_scope.clone();
    admission.effect_receipt_template.scope = recovery_scope.clone();
    admission.recovery = AutomationRecoveryBinding::External {
        recovery_problem: reset_problem(&admission.request_id, &recovery_scope, &admission.request),
    };
    seal_effect_authority(admission)
}

fn admission_for_recovery_project(
    cg: &tracedecay_project::project::TraceDecay,
    run_id: &str,
    request_id: &str,
) -> DurableAutomationAdmission {
    let owner = cg.project_memory_owner().expect("project memory owner");
    let FactOwnerV1::Project { project_id } = owner.clone() else {
        panic!("automation recovery fixture requires a project owner")
    };
    let recovery_scope =
        tracedecay_code_index_runtime::resolved_scope_for_project(cg.project_root(), &project_id)
            .expect("recovery scope");
    let mut admission = admission(run_id, request_id);
    admission.scope = recovery_scope.clone();
    admission.effect_receipt_template.scope = recovery_scope.clone();
    admission.recovery = AutomationRecoveryBinding::Memory {
        owner,
        recovery_problem: reset_problem(&admission.request_id, &recovery_scope, &admission.request),
    };
    seal_effect_authority(admission)
}

async fn retained_external_authority(
    runs: &RegisteredAutomationRunStore,
    admission: DurableAutomationAdmission,
) -> (
    AutomationEffectAuthority,
    std::path::PathBuf,
    DurableAutomationAdmission,
) {
    use std::collections::BTreeSet;

    use tracedecay_contracts::{
        CancellationContext, CancellationSignal, CapabilityGrantSnapshot, RequestContext,
    };

    let operation =
        retained_surface_application_operation(RetainedSurfaceOperation::FactStoreCurate)
            .expect("retained operation");
    let cancellation_id = format!("cancel.{}", admission.request_id.as_str());
    let grant = CapabilityGrantSnapshot::new(
        admission.grant_id.clone(),
        admission.grant_revision,
        admission.grant_digest.clone(),
        admission.actor.clone(),
        UtcMicros(1),
        UtcMicros(i64::MAX - 1),
        admission.scope.clone(),
        BTreeSet::from([operation.capability_id().clone()]),
        BTreeSet::from([operation.use_case_id().clone()]),
        admission.disclosure,
    )
    .expect("grant");
    let context = RequestContext::new(
        admission.actor.clone(),
        admission.scope.clone(),
        grant,
        admission.request_id.clone(),
        Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
        CancellationContext::active(cancellation_id.clone()).expect("cancellation context"),
    )
    .expect("request context");
    let cancellation = CancellationSignal::active(cancellation_id).expect("cancellation");
    let AutomationEffectAdmission::Execute(authority) =
        Box::pin(AutomationEffectAuthority::prepare(
            AdmittedAutomationEffectRequest {
                context,
                cancellation,
                observed_at: UtcMicros(2),
                configuration_digest: admission.configuration_digest.clone(),
                request: admission.request.clone(),
                runs: runs.clone(),
            },
            || {
                Ok(FactOwnerV1::Project {
                    project_id: admission.scope.project_id.clone(),
                })
            },
            |_, _| async { Err(contract_error("fresh admission must not recover receipts")) },
        ))
        .await
        .expect("durable admission")
    else {
        panic!("fresh retained fixture must execute")
    };
    let journal_path = canonical_journal_path(runs.dashboard_root(), &admission.request.run_id);
    let expected_admission = read_indexed_record_blocking(&journal_path)
        .expect("admitted journal")
        .expect("journal")
        .admission()
        .clone();
    (*authority, journal_path, expected_admission)
}

async fn retained_recovery_project(
    temp: &tempfile::TempDir,
    name: &str,
) -> tracedecay_project::project::TraceDecay {
    let project_root = temp.path().join(format!("{name}-project"));
    let profile_root = temp.path().join(format!("{name}-profile"));
    std::fs::create_dir_all(project_root.join("src")).expect("project source directory");
    std::fs::write(project_root.join("src/lib.rs"), "pub fn fixture() {}\n")
        .expect("project source");
    tracedecay_project::project::TraceDecay::init_with_options_for_test(
        &project_root,
        tracedecay_project::project::TraceDecayOpenOptions {
            profile_root: Some(profile_root.clone()),
            global_db_path: Some(profile_root.join("global.db")),
        },
    )
    .await
    .expect("initialize automation recovery project")
}

async fn fixed_task_lock_is_denied(
    dashboard_root: &std::path::Path,
    task: tracedecay_automation_runtime::automation::backend::AgentTaskKind,
) -> bool {
    tracedecay_automation_runtime::automation::scheduler::AutomationTaskLock::try_acquire(
        dashboard_root,
        task,
    )
    .await
    .expect("competing fixed-task lock")
    .is_none()
}

async fn retained_repeated_memory_curator(
    cg: &tracedecay_project::project::TraceDecay,
    config: &tracedecay_automation_runtime::automation::config::AutomationSettingsV1,
    configuration_revision: &tracedecay_domain::configuration::ConfigurationRevisionId,
    run_id: &str,
) -> (
    tracedecay_automation_runtime::automation::runner::ReusedSchedulerSkip,
    tracedecay_automation_runtime::automation::runner::AutomationRunSettlementGuard,
) {
    use tracedecay_automation_runtime::automation::runner::RetainedAutomationSettlementDisposition;

    let retained =
        retained_repeated_memory_curator_run(cg, config, configuration_revision, run_id).await;
    match retained.into_settlement_disposition() {
        RetainedAutomationSettlementDisposition::ReusedSchedulerSkip {
            reused,
            settlement_guard,
        } => (reused, settlement_guard),
        RetainedAutomationSettlementDisposition::Current { .. } => {
            panic!("fixed-task scheduler repeat must retain its exact prior skip")
        }
    }
}

async fn retained_repeated_memory_curator_run(
    cg: &tracedecay_project::project::TraceDecay,
    config: &tracedecay_automation_runtime::automation::config::AutomationSettingsV1,
    configuration_revision: &tracedecay_domain::configuration::ConfigurationRevisionId,
    run_id: &str,
) -> tracedecay_automation_runtime::automation::runner::RetainedAutomationRun<
    tracedecay_automation_runtime::automation::runner::MemoryCuratorAutomationRun,
> {
    use std::sync::Arc;
    use tracedecay_automation_runtime::automation::AutomationRunControl;
    use tracedecay_automation_runtime::automation::run_ledger::AutomationTrigger;
    use tracedecay_automation_runtime::automation::runner::{
        MemoryCuratorAutomationOptions, run_memory_curator_with_backend_for_retained_settlement,
    };

    let run_control = AutomationRunControl::from_interrupted(Arc::new(|| false));
    let project_context = cg
        .automation_project_context(None)
        .expect("compose project automation context");
    run_memory_curator_with_backend_for_retained_settlement(
        &project_context,
        config,
        configuration_revision,
        &NeverAutomationBackend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            run_id: Some(run_id.to_owned()),
            ..MemoryCuratorAutomationOptions::default()
        },
        &run_control,
    )
    .await
}

fn partial_receipt_template(request_id: &RequestId, scope: &ResolvedScope) -> EffectReceipt {
    let operation =
        retained_surface_application_operation(RetainedSurfaceOperation::FactStoreCurate)
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

#[cfg(unix)]
fn authority(scope: &ResolvedScope) -> AuthorityReceipt {
    AuthorityReceipt {
        grant_id: tracedecay_contracts::CapabilityGrantId::new("grant.memory-journal")
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

#[cfg(unix)]
fn success_terminal(
    admission: &DurableAutomationAdmission,
    result_run_id: &str,
) -> AutomationSettledTerminal {
    result_terminal(
        admission,
        result_run_id,
        AutomationTaskV1::MemoryCurator,
        AutomationRunTerminalV1::Completed {
            summary: AutomationRunSummaryV1 {
                reviewed_count: 0,
                accepted_count: 0,
                rejected_count: 0,
                skipped_count: 0,
            },
        },
    )
}

#[cfg(unix)]
fn result_terminal(
    admission: &DurableAutomationAdmission,
    result_run_id: &str,
    task: AutomationTaskV1,
    terminal: AutomationRunTerminalV1,
) -> AutomationSettledTerminal {
    let operation =
        retained_surface_application_operation(RetainedSurfaceOperation::FactStoreCurate)
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
        task,
        request_digest: admission.request.input_digest().expect("request digest"),
        terminal,
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
            OperationBudgetUsage::default(),
        )
        .expect("execution"),
        ReconciliationState::Reconciled,
        receipt,
        Some(result),
    )
    .expect("effect result");
    AutomationSettledTerminal::Outcome {
        scope: admission.scope.clone(),
        outcome: Box::new(ApplicationOutcome::Effect(effect)),
    }
}

#[cfg(unix)]
#[test]
fn scheduler_stable_request_identity_reopens_the_same_terminal() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("terminal.json");
    let first_request =
        crate::automation_effect::scheduler_automation_request_id(Some("host_receipt_17"))
            .expect("first scheduler identity");
    let reopened_request =
        crate::automation_effect::scheduler_automation_request_id(Some("host_receipt_17"))
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

#[tokio::test]
async fn post_write_reservation_error_retains_reserved_journal_and_pending_recovery() {
    let temp = tempfile::tempdir().expect("tempdir");
    let cg = retained_recovery_project(&temp, "post-write-reservation").await;
    let dashboard_root = &cg.store_layout().dashboard_root;
    let admission = external_admission_for_recovery_project(
        &cg,
        "run.post-write-reservation",
        "request.post-write-reservation",
        "post-write-reservation",
    );
    let path = canonical_journal_path(dashboard_root, &admission.request.run_id);

    let reservation = reserve_or_replay_with_index_and_writer(
        &path,
        admission.clone(),
        || recovery_index::add_pending_blocking(dashboard_root, &path, &admission),
        |path, record| {
            write_record(path, record)?;
            Err(contract_error("injected error after exact Reserved write"))
        },
    );
    let reservation_error = match reservation {
        Ok(_) => panic!("post-write error must remain uncertain"),
        Err(error) => error,
    };
    assert!(
        reservation_error
            .to_string()
            .contains("after exact Reserved write")
    );
    let reserved = read_indexed_record_blocking(&path)
        .expect("physical journal read")
        .expect("physical Reserved journal");
    assert_eq!(reserved.admission(), &admission);
    assert!(matches!(reserved.state, DurableAutomationState::Reserved));
    let pending = recovery_index::indexed_journals_blocking(dashboard_root, &admission.scope)
        .expect("pending index");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].path, path);

    let report = recovery_composition::reconcile_reserved_automation_effects_for_project(
        &cg,
        &tracedecay_contracts::CancellationSignal::active("cancellation.post-write-reservation")
            .expect("recovery cancellation"),
    )
    .await
    .expect("recover physical Reserved journal");
    assert_eq!(report.inspected, 1);
    assert_eq!(report.indeterminate, 1);
    assert!(
        read_indexed_record_blocking(&path)
            .expect("recovered journal")
            .expect("terminal journal")
            .is_terminal()
    );
    assert!(
        recovery_index::indexed_journals_blocking(dashboard_root, &admission.scope)
            .expect("closed pending index")
            .is_empty()
    );
}

#[tokio::test]
async fn prewrite_reservation_error_retains_index_until_missing_journal_recovery() {
    let temp = tempfile::tempdir().expect("tempdir");
    let cg = retained_recovery_project(&temp, "prewrite-reservation").await;
    let dashboard_root = &cg.store_layout().dashboard_root;
    let admission = external_admission_for_recovery_project(
        &cg,
        "run.prewrite-reservation",
        "request.prewrite-reservation",
        "prewrite-reservation",
    );
    let path = canonical_journal_path(dashboard_root, &admission.request.run_id);

    let reservation = reserve_or_replay_with_index_and_writer(
        &path,
        admission.clone(),
        || recovery_index::add_pending_blocking(dashboard_root, &path, &admission),
        |_path, _record| Err(contract_error("injected error before Reserved write")),
    );
    let reservation_error = match reservation {
        Ok(_) => panic!("prewrite error must retain recovery authority"),
        Err(error) => error,
    };
    assert!(
        reservation_error
            .to_string()
            .contains("before Reserved write")
    );
    assert!(
        read_indexed_record_blocking(&path)
            .expect("missing journal read")
            .is_none()
    );
    let pending = recovery_index::indexed_journals_blocking(dashboard_root, &admission.scope)
        .expect("pending index");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].path, path);

    let report = recovery_composition::reconcile_reserved_automation_effects_for_project(
        &cg,
        &tracedecay_contracts::CancellationSignal::active("cancellation.prewrite-reservation")
            .expect("recovery cancellation"),
    )
    .await
    .expect("recover missing journal index");
    assert_eq!(report.inspected, 1);
    assert_eq!(report.already_terminal, 1);
    assert!(
        read_indexed_record_blocking(&path)
            .expect("journal remains absent")
            .is_none()
    );
    assert!(
        recovery_index::indexed_journals_blocking(dashboard_root, &admission.scope)
            .expect("closed pending index")
            .is_empty()
    );
}

#[tokio::test]
async fn recovery_defers_unavailable_memory_without_blocking_external_or_terminal_effects() {
    let temp = tempfile::tempdir().expect("tempdir");
    let fixture_name = "unavailable-memory-recovery";
    let project_root = temp.path().join(format!("{fixture_name}-project"));
    let profile_root = temp.path().join(format!("{fixture_name}-profile"));
    let cg = retained_recovery_project(&temp, fixture_name).await;
    let dashboard_root = cg.store_layout().dashboard_root.clone();
    let admission = admission_for_recovery_project(
        &cg,
        "run.unavailable-memory-recovery",
        "request.unavailable-memory-recovery",
    );
    let journal_path = canonical_journal_path(&dashboard_root, &admission.request.run_id);
    recovery_index::add_pending_blocking(&dashboard_root, &journal_path, &admission)
        .expect("pending memory recovery");
    reserve_or_replay_blocking(&journal_path, admission).expect("reserved memory recovery");
    let external = external_admission_for_recovery_project(
        &cg,
        "run.read-only-external",
        "request.read-only-external",
        "read-only-external",
    );
    let external_path = canonical_journal_path(&dashboard_root, &external.request.run_id);
    recovery_index::add_pending_blocking(&dashboard_root, &external_path, &external)
        .expect("external index");
    reserve_or_replay_blocking(&external_path, external).expect("external reservation");
    let terminal = external_admission_for_recovery_project(
        &cg,
        "run.read-only-terminal",
        "request.read-only-terminal",
        "read-only-terminal",
    );
    let terminal_path = canonical_journal_path(&dashboard_root, &terminal.request.run_id);
    recovery_index::add_pending_blocking(&dashboard_root, &terminal_path, &terminal)
        .expect("terminal index");
    reserve_or_replay_blocking(&terminal_path, terminal.clone()).expect("terminal reservation");
    persist_recovered_terminal_blocking(
        &terminal_path,
        &terminal,
        AutomationSettledTerminal::Problem(terminal.recovery_problem().clone()),
        None,
    )
    .expect("terminal journal");
    cg.close();

    let read_only = tracedecay_project::project::TraceDecay::open_read_only_with_options_for_test(
        &project_root,
        tracedecay_project::project::TraceDecayOpenOptions {
            profile_root: Some(profile_root.clone()),
            global_db_path: Some(profile_root.join("global.db")),
        },
    )
    .await
    .expect("open read-only recovery project");
    let report = recovery_composition::reconcile_reserved_automation_effects_for_project(
        &read_only,
        &tracedecay_contracts::CancellationSignal::active(
            "cancellation.unavailable-memory-recovery",
        )
        .expect("recovery cancellation"),
    )
    .await
    .expect("memory failure must defer only the memory journal");

    assert_eq!(report.inspected, 3);
    assert_eq!(report.deferred, 1);
    assert_eq!(report.indeterminate, 1);
    assert_eq!(report.already_terminal, 1);
    assert!(
        read_indexed_record_blocking(&external_path)
            .expect("external journal")
            .expect("external terminal")
            .is_terminal()
    );
    assert!(
        !read_indexed_record_blocking(&journal_path)
            .expect("memory journal")
            .expect("reserved memory")
            .is_terminal()
    );
}

#[tokio::test]
async fn empty_pending_index_does_not_open_project_memory() {
    let temp = tempfile::tempdir().expect("tempdir");
    let fixture_name = "empty-index-with-unavailable-memory";
    let project_root = temp.path().join(format!("{fixture_name}-project"));
    let profile_root = temp.path().join(format!("{fixture_name}-profile"));
    let cg = retained_recovery_project(&temp, fixture_name).await;
    cg.close();

    let read_only = tracedecay_project::project::TraceDecay::open_read_only_with_options_for_test(
        &project_root,
        tracedecay_project::project::TraceDecayOpenOptions {
            profile_root: Some(profile_root.clone()),
            global_db_path: Some(profile_root.join("global.db")),
        },
    )
    .await
    .expect("open read-only recovery project");
    let report = recovery_composition::reconcile_reserved_automation_effects_for_project(
        &read_only,
        &tracedecay_contracts::CancellationSignal::active(
            "cancellation.empty-index-with-unavailable-memory",
        )
        .expect("recovery cancellation"),
    )
    .await
    .expect("empty recovery must not open project memory");

    assert_eq!(
        report,
        recovery_index::AutomationEffectRecoveryReport::default()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reused_scheduler_skip_abandons_current_effect_before_observing_exact_prior() {
    use std::sync::Arc;
    use std::time::Duration;
    use tracedecay_automation_runtime::automation::AutomationRunControl;
    use tracedecay_automation_runtime::automation::backend::{AgentTaskKind, task_key};
    use tracedecay_automation_runtime::automation::config::{
        AutomationBackendV1, AutomationHostModeV1, AutomationSettingsV1,
    };
    use tracedecay_automation_runtime::automation::run_ledger::AutomationTrigger;
    use tracedecay_automation_runtime::automation::runner::{
        MemoryCuratorAutomationOptions, RetainedAutomationSettlementDisposition,
        run_memory_curator_with_backend_for_retained_settlement,
    };
    use tracedecay_domain::configuration::ConfigurationRevisionId;

    let temp = tempfile::tempdir().expect("tempdir");
    let project_root = temp.path().join("project");
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(project_root.join("src")).expect("project source directory");
    std::fs::write(project_root.join("src/lib.rs"), "pub fn fixture() {}\n")
        .expect("project source");
    let cg = tracedecay_project::project::TraceDecay::init_with_options_for_test(
        &project_root,
        tracedecay_project::project::TraceDecayOpenOptions {
            profile_root: Some(profile_root.clone()),
            global_db_path: Some(profile_root.join("global.db")),
        },
    )
    .await
    .expect("initialize fixed-task automation project");
    let project_context = cg
        .automation_project_context(None)
        .expect("compose project automation context");
    let runs = cg.automation_run_store().expect("registered run store");
    let dashboard_root = runs.dashboard_root();
    let config = AutomationSettingsV1 {
        enabled: true,
        backend: AutomationBackendV1::Disabled,
        host_mode: AutomationHostModeV1::Standalone,
        model_id: None,
        ..AutomationSettingsV1::default()
    };
    let configuration_revision =
        ConfigurationRevisionId::new("configuration.reused-scheduler-skip")
            .expect("configuration revision");
    let run_control = AutomationRunControl::from_interrupted(Arc::new(|| false));
    let prior_run_id = "run.reused-scheduler-skip.prior";
    let current_run_id = "run.reused-scheduler-skip.current";

    let prior = run_memory_curator_with_backend_for_retained_settlement(
        &project_context,
        &config,
        &configuration_revision,
        &NeverAutomationBackend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            run_id: Some(prior_run_id.to_owned()),
            ..MemoryCuratorAutomationOptions::default()
        },
        &run_control,
    )
    .await;
    let (prior_run, prior_guard) = match prior.into_settlement_disposition() {
        RetainedAutomationSettlementDisposition::Current {
            result,
            settlement_guard,
        } => {
            let run = match result {
                Ok(run) => run,
                Err(error) => panic!("fixed-task prior skip failed: {error}"),
            };
            (run, settlement_guard)
        }
        RetainedAutomationSettlementDisposition::ReusedSchedulerSkip { .. } => {
            panic!("first fixed-task scheduler skip must be current")
        }
    };
    let prior_record = prior_run.ledger_record.clone();
    let (prior_authority, prior_journal, prior_admission) = retained_external_authority(
        &runs,
        admission(prior_run_id, "request.reused-scheduler-skip.prior"),
    )
    .await;
    recovery_index::add_pending_blocking(dashboard_root, &prior_journal, &prior_admission)
        .expect("prior pending authority");
    prior_authority
        .start_deferred_run_settlement_observed(
            prior_run.ledger_record,
            prior_run.committed_receipt,
            prior_guard,
            None,
        )
        .wait()
        .await
        .expect("settle exact prior scheduler skip");

    let prior_history = runs
        .load_run_records(10)
        .await
        .expect("prior exact history");
    assert_eq!(
        runs.find_run_record(prior_run_id)
            .await
            .expect("prior exact lookup"),
        Some(prior_record.clone())
    );
    assert_eq!(prior_record.task, AgentTaskKind::MemoryCurator);
    assert_eq!(
        prior_record.task_key,
        task_key(AgentTaskKind::MemoryCurator)
    );
    assert_eq!(prior_record.error.as_deref(), Some("backend_disabled"));

    let wrong_task_run_id = "run.reused-scheduler-skip.wrong-task";
    let (mut wrong_task_reused, wrong_task_guard) =
        retained_repeated_memory_curator(&cg, &config, &configuration_revision, wrong_task_run_id)
            .await;
    wrong_task_reused.task_key = task_key(AgentTaskKind::SessionReflector).to_owned();
    let (wrong_task_authority, wrong_task_journal, wrong_task_admission) =
        retained_external_authority(
            &runs,
            admission(
                wrong_task_run_id,
                "request.reused-scheduler-skip.wrong-task",
            ),
        )
        .await;
    recovery_index::add_pending_blocking(
        dashboard_root,
        &wrong_task_journal,
        &wrong_task_admission,
    )
    .expect("wrong-task pending authority");
    let wrong_task_observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let wrong_task_observer = Arc::clone(&wrong_task_observed);
    let Err(wrong_task_error) = wrong_task_authority
        .start_reused_scheduler_skip_abandonment_observed(
            wrong_task_reused,
            wrong_task_guard,
            Some(Box::new(move |_| {
                wrong_task_observer.store(true, std::sync::atomic::Ordering::SeqCst);
            })),
        )
    else {
        panic!("wrong fixed-task identity must reject before abandonment")
    };
    assert!(!wrong_task_observed.load(std::sync::atomic::Ordering::SeqCst));
    assert!(wrong_task_journal.exists());
    assert!(fixed_task_lock_is_denied(dashboard_root, AgentTaskKind::MemoryCurator).await);
    assert_eq!(
        runs.load_run_records(10)
            .await
            .expect("history after wrong-task rejection"),
        prior_history
    );
    drop(wrong_task_error);
    assert!(!fixed_task_lock_is_denied(dashboard_root, AgentTaskKind::MemoryCurator).await);
    abandon_reservation_blocking(&wrong_task_journal, &wrong_task_admission)
        .expect("clean wrong-task reservation");
    recovery_index::remove_pending_blocking(dashboard_root, &wrong_task_journal)
        .expect("clean wrong-task pending authority");

    let wrong_reason_run_id = "run.reused-scheduler-skip.wrong-reason";
    let (mut wrong_reason_reused, wrong_reason_guard) = retained_repeated_memory_curator(
        &cg,
        &config,
        &configuration_revision,
        wrong_reason_run_id,
    )
    .await;
    wrong_reason_reused.reason = "different_skip_reason".to_owned();
    let (wrong_reason_authority, wrong_reason_journal, wrong_reason_admission) =
        retained_external_authority(
            &runs,
            admission(
                wrong_reason_run_id,
                "request.reused-scheduler-skip.wrong-reason",
            ),
        )
        .await;
    recovery_index::add_pending_blocking(
        dashboard_root,
        &wrong_reason_journal,
        &wrong_reason_admission,
    )
    .expect("wrong-reason pending authority");
    let Err(wrong_reason_error) = wrong_reason_authority
        .start_reused_scheduler_skip_abandonment_observed(
            wrong_reason_reused,
            wrong_reason_guard,
            None,
        )
    else {
        panic!("wrong skip reason must reject before abandonment")
    };
    assert!(wrong_reason_journal.exists());
    assert!(fixed_task_lock_is_denied(dashboard_root, AgentTaskKind::MemoryCurator).await);
    assert_eq!(
        runs.load_run_records(10)
            .await
            .expect("history after wrong-reason rejection"),
        prior_history
    );
    drop(wrong_reason_error);
    assert!(!fixed_task_lock_is_denied(dashboard_root, AgentTaskKind::MemoryCurator).await);
    abandon_reservation_blocking(&wrong_reason_journal, &wrong_reason_admission)
        .expect("clean wrong-reason reservation");
    recovery_index::remove_pending_blocking(dashboard_root, &wrong_reason_journal)
        .expect("clean wrong-reason pending authority");

    let current_retained =
        retained_repeated_memory_curator_run(&cg, &config, &configuration_revision, current_run_id)
            .await;
    let (current_authority, current_journal, current_admission) = retained_external_authority(
        &runs,
        admission(current_run_id, "request.reused-scheduler-skip.current"),
    )
    .await;
    recovery_index::add_pending_blocking(dashboard_root, &current_journal, &current_admission)
        .expect("current pending authority");

    let journal_lock_path = tracedecay_runtime_core::storage::append_lock_path(&current_journal);
    let journal_lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&journal_lock_path)
        .expect("current journal lock");
    journal_lock.lock().expect("block abandonment");
    let (observed_tx, observed_rx) = std::sync::mpsc::channel();
    let (projected_tx, projected_rx) = std::sync::mpsc::channel();
    let waiter = current_authority.start_retained_automation_settlement(
        current_retained,
        Some(Box::new(move |record| {
            observed_tx
                .send(record.clone())
                .expect("observe reused scheduler skip");
        })),
        move |run| {
            projected_tx
                .send(())
                .expect("project current retained automation run");
            (run.ledger_record, run.committed_receipt)
        },
    );
    drop(waiter);

    assert!(
        observed_rx
            .recv_timeout(Duration::from_millis(100))
            .is_err(),
        "observer cannot run before current abandonment is durable"
    );
    assert!(current_journal.exists());
    assert!(
        runs.find_run_record(current_run_id)
            .await
            .expect("current exact absence")
            .is_none()
    );
    assert!(fixed_task_lock_is_denied(dashboard_root, AgentTaskKind::MemoryCurator).await);

    journal_lock.unlock().expect("release abandonment");
    assert_eq!(
        observed_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("detached prior observation"),
        prior_record
    );
    assert!(observed_rx.try_recv().is_err(), "prior is observed once");
    assert!(
        projected_rx.try_recv().is_err(),
        "reused scheduler skip must not project a current run"
    );
    assert!(!current_journal.exists());
    assert!(
        !terminal_sidecar_path(&current_journal)
            .expect("current terminal sidecar")
            .exists()
    );
    assert!(
        recovery_index::indexed_journals_blocking(dashboard_root, &current_admission.scope)
            .expect("pending index after abandonment")
            .is_empty()
    );
    assert_eq!(
        runs.load_run_records(10)
            .await
            .expect("history after abandonment"),
        prior_history,
        "reusing a scheduler skip must not publish a current run"
    );
    assert!(
        runs.find_run_record(current_run_id)
            .await
            .expect("current exact absence after abandonment")
            .is_none()
    );
    assert!(
        tracedecay_automation_runtime::automation::scheduler::AutomationTaskLock::try_acquire(
            dashboard_root,
            AgentTaskKind::MemoryCurator,
        )
        .await
        .expect("post-abandonment task lock")
        .is_some()
    );
}
