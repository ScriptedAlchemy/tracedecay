use tracedecay_automation_runtime::automation::effect_runtime::contract_error;
use tracedecay_automation_runtime::automation::effect_runtime::settlement::*;

use std::collections::BTreeMap;
use tracedecay_contracts::retained_surfaces::{
    AutomationRunProblemV1, AutomationRunRequestV1, AutomationRunResultV1, AutomationRunSummaryV1,
    AutomationRunTerminalV1, AutomationTaskRequestV1, AutomationTaskV1, MemoryCuratorRunInputV1,
    RetainedSurfaceExecutionErrorV1, RetainedSurfaceOperation, RetainedSurfaceResultV1,
    UserJobRunInputV1, retained_surface_application_operation, retained_surface_execution_problem,
};

use tracedecay_contracts::{
    ApplicationOutcome, ApplicationProblemEnvelope, AuthorityReceipt, Deadline, DisclosureClass,
    EffectId, EffectReceipt, EffectResult, EffectTermination, IdempotencyKey, OperationBudgetUsage,
    OperationReceipt, PolicyDecisionRef, ReconciliationState, RequestId, ResolvedScope,
};
use tracedecay_domain::{
    ActorId, ComponentVersion, FactOwnerV1, ManifestDigest, ProjectId, RepositoryId, RunId,
    UtcMicros, WorktreeId, canonical_sha256,
};
use tracedecay_tool_catalog::EffectClass;

use crate::daemon::automation_effect::recovery_composition;
use tracedecay_automation_runtime::automation::effect_runtime::AutomationSettledTerminal;
use tracedecay_automation_runtime::automation::effect_runtime::journal::*;
use tracedecay_automation_runtime::automation::effect_runtime::recovery_index;

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
}

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
            retirement: None,
            reset_source_digest: None,
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
    cg: &crate::tracedecay::TraceDecay,
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
    cg: &crate::tracedecay::TraceDecay,
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
        retirement: None,
        reset_source_digest: None,
    };
    seal_effect_authority(admission)
}

fn retirement_admission_for_recovery_project(
    cg: &crate::tracedecay::TraceDecay,
    run_id: &str,
    request_id: &str,
    binding: tracedecay_automation_runtime::automation::effect_runtime::retirement::RetirementBinding,
) -> DurableAutomationAdmission {
    let owner = cg.project_memory_owner().expect("project memory owner");
    let FactOwnerV1::Project { project_id } = owner.clone() else {
        panic!("automation retirement fixture requires a project owner")
    };
    let recovery_scope =
        tracedecay_code_index_runtime::resolved_scope_for_project(cg.project_root(), &project_id)
            .expect("retirement recovery scope");
    let mut admission = admission(run_id, request_id);
    admission.request.task = AutomationTaskRequestV1::SessionReflector(
        tracedecay_contracts::retained_surfaces::SessionReflectorRunInputV1 {
            provider: "cursor".to_owned(),
            query: "retire exact shipped proposal history".to_owned(),
            scope: tracedecay_contracts::retained_surfaces::LcmSearchScopeV1::Current,
            session_id: None,
            include_summaries: true,
            evidence_limit: 5,
            include_recent_sessions: false,
            recent_sessions_limit: 1,
            sort: tracedecay_contracts::retained_surfaces::LcmGrepSortV1::Recency,
            source: None,
            role: None,
            start_time: None,
            end_time: None,
        },
    );
    admission.scope = recovery_scope.clone();
    admission.effect_receipt_template.scope = recovery_scope.clone();
    admission.recovery = AutomationRecoveryBinding::Memory {
        owner,
        recovery_problem: reset_problem(&admission.request_id, &recovery_scope, &admission.request),
        retirement: Some(binding),
        reset_source_digest: None,
    };
    seal_effect_authority(admission)
}

fn write_private_test_file(path: &std::path::Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create private test file parent");
    }
    std::fs::write(path, bytes).expect("write private test file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .expect("private test file mode");
    }
    #[cfg(windows)]
    drop(
        tracedecay_runtime_core::windows_security::make_private_file(path)
            .expect("private test file ACL"),
    );
}

async fn retained_external_authority(
    dashboard_root: &std::path::Path,
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
                dashboard_root: dashboard_root.to_path_buf(),
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
    let journal_path = canonical_journal_path(dashboard_root, &admission.request.run_id);
    let expected_admission = read_indexed_record_blocking(&journal_path)
        .expect("admitted journal")
        .expect("journal")
        .admission()
        .clone();
    (*authority, journal_path, expected_admission)
}

async fn retained_disabled_user_job(
    dashboard_root: &std::path::Path,
    run_id: &str,
    job_id: &str,
) -> (
    tracedecay_automation_runtime::automation::jobs::UserJobAutomationRun,
    tracedecay_automation_runtime::automation::runner::AutomationRunSettlementGuard,
) {
    let retained = retained_disabled_user_job_run(dashboard_root, run_id, job_id).await;
    let (result, guard) = retained.into_parts();
    (result.expect("disabled retained job terminal"), guard)
}

async fn retained_disabled_user_job_run(
    dashboard_root: &std::path::Path,
    run_id: &str,
    job_id: &str,
) -> tracedecay_automation_runtime::automation::runner::RetainedAutomationRun<
    tracedecay_automation_runtime::automation::jobs::UserJobAutomationRun,
> {
    use tracedecay_automation_runtime::automation::config::{
        AutomationBackend, AutomationConfig, AutomationHostMode,
    };
    use tracedecay_automation_runtime::automation::jobs::{
        AutomationJob, JobDelivery, UserJobRunOptions,
        run_user_job_with_backend_for_retained_settlement,
    };

    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        ..AutomationConfig::default()
    };
    let job = AutomationJob {
        id: job_id.to_owned(),
        name: format!("{job_id} retained test"),
        prompt: "This disabled job must stop after acquiring its canonical lock.".to_owned(),
        schedule: None,
        enabled: false,
        interval_secs: None,
        cooldown_secs: None,
        skill_ids: Vec::new(),
        pre_run_command: None,
        delivery: JobDelivery::default(),
        created_at: 1,
        updated_at: 1,
        extra: BTreeMap::default(),
    };
    run_user_job_with_backend_for_retained_settlement(
        dashboard_root,
        &config,
        &NeverAutomationBackend,
        &job,
        UserJobRunOptions {
            run_id: Some(run_id.to_owned()),
            ..UserJobRunOptions::default()
        },
    )
    .await
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("wall clock")
        .as_secs() as i64
}

async fn retained_recovery_project(
    temp: &tempfile::TempDir,
    name: &str,
) -> crate::tracedecay::TraceDecay {
    let project_root = temp.path().join(format!("{name}-project"));
    let profile_root = temp.path().join(format!("{name}-profile"));
    std::fs::create_dir_all(project_root.join("src")).expect("project source directory");
    std::fs::write(project_root.join("src/lib.rs"), "pub fn fixture() {}\n")
        .expect("project source");
    crate::tracedecay::TraceDecay::init_with_options(
        &project_root,
        crate::tracedecay::TraceDecayOpenOptions {
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
        None,
        now_secs(),
    )
    .await
    .expect("competing fixed-task lock")
    .is_none()
}

async fn retained_repeated_memory_curator(
    cg: &crate::tracedecay::TraceDecay,
    config: &tracedecay_automation_runtime::automation::config::AutomationConfig,
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
    cg: &crate::tracedecay::TraceDecay,
    config: &tracedecay_automation_runtime::automation::config::AutomationConfig,
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
        .automation_project_context()
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

fn exact_spool_files(dashboard_root: &std::path::Path) -> Vec<std::path::PathBuf> {
    match std::fs::read_dir(dashboard_root.join("automation_run_spool")) {
        Ok(entries) => entries
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .map(|entry| entry.path())
            .collect(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => panic!("read exact spool directory: {error}"),
    }
}

fn exact_spool_file_count(dashboard_root: &std::path::Path) -> usize {
    exact_spool_files(dashboard_root).len()
}

fn retirement_capture_count(dashboard_root: &std::path::Path) -> usize {
    std::fs::read_dir(dashboard_root)
        .expect("retirement capture inventory")
        .filter_map(std::result::Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".fact_proposals.retirement-")
        })
        .count()
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

fn retirement_terminal(admission: &DurableAutomationAdmission) -> AutomationSettledTerminal {
    result_terminal(
        admission,
        admission.request.run_id.as_str(),
        AutomationTaskV1::SessionReflector,
        AutomationRunTerminalV1::Skipped {
            reason:
                tracedecay_contracts::retained_surfaces::AutomationSkipReasonV1::from_ledger_reason(
                    "shipped_fact_proposal_history_retired",
                )
                .expect("retirement skip reason"),
            summary: AutomationRunSummaryV1 {
                reviewed_count: 0,
                accepted_count: 0,
                rejected_count: 0,
                skipped_count: 1,
            },
        },
    )
}

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
        Some(RetainedSurfaceResultV1::FactStoreCurate(result)),
    )
    .expect("effect result");
    AutomationSettledTerminal::Outcome {
        scope: admission.scope.clone(),
        outcome: Box::new(ApplicationOutcome::Effect(effect)),
    }
}

#[tokio::test]
async fn terminal_retirement_recovery_keeps_pending_until_source_is_exactly_archived() {
    let temp = tempfile::tempdir().expect("tempdir");
    let fixture_name = "terminal-retirement-recovery";
    let cg = retained_recovery_project(&temp, fixture_name).await;
    let dashboard_root = cg.store_layout().dashboard_root.clone();
    let project_root = cg.project_root().to_path_buf();
    let profile_root = temp.path().join(format!("{fixture_name}-profile"));
    let source_path = dashboard_root.join("fact_proposals.json");
    let source_bytes = br#"{"schema_version":1,"proposals":[]}"#.to_vec();
    write_private_test_file(&source_path, &source_bytes);
    let plan = match tracedecay_automation_runtime::automation::effect_runtime::retirement::classify_for_task(
        AutomationTaskV1::SessionReflector,
        &dashboard_root,
    )
    .await
    .expect("classify exact retirement source")
    {
        tracedecay_automation_runtime::automation::effect_runtime::retirement::RetirementClassification::Terminal(plan) => plan,
        _ => panic!("terminal shipped history must yield an exact retirement plan"),
    };
    let binding = plan.binding.clone();
    let archive_path = dashboard_root
        .join("fact_proposals.archive")
        .join(&binding.archive_name);
    let admission = retirement_admission_for_recovery_project(
        &cg,
        "run.terminal-retirement-recovery",
        "request.terminal-retirement-recovery",
        binding,
    );
    let journal_path = canonical_journal_path(&dashboard_root, &admission.request.run_id);

    let (anchor, anchor_guard) = retained_disabled_user_job(
        &dashboard_root,
        "run.terminal-retirement-ledger-anchor",
        "terminal-retirement-ledger-anchor",
    )
    .await;
    drop(anchor_guard);
    let (anchor_publication, _) =
        tracedecay_automation_runtime::automation::run_ledger::bind_staged_run_record_exact(
            &dashboard_root,
            &anchor.ledger_record,
            |publication| Ok(publication.clone()),
        )
        .expect("stage exact anchor ledger row");
    assert_eq!(
        tracedecay_automation_runtime::automation::run_ledger::publish_staged_run_record_exact(
            &dashboard_root,
            &anchor.ledger_record.run_id,
            &anchor_publication,
        )
        .await
        .expect("publish exact anchor ledger row"),
        tracedecay_automation_runtime::automation::run_ledger::ExactRunPublishOutcome::Published
    );
    tracedecay_automation_runtime::automation::run_ledger::discard_staged_run_record_exact(
        &dashboard_root,
        &anchor.ledger_record.run_id,
        &anchor_publication,
    )
    .await
    .expect("retire exact anchor spool");

    let claim = match reserve_or_replay_indexed_blocking(
        &journal_path,
        admission.clone(),
        || recovery_index::add_pending_blocking(&dashboard_root, &journal_path, &admission),
        || recovery_index::remove_pending_blocking(&dashboard_root, &journal_path),
    )
    .expect("reserve indexed retirement")
    {
        ReservationResult::Execute { claim, retirement } => {
            assert_eq!(retirement, admission.retirement().cloned());
            claim
        }
        _ => panic!("fresh retirement admission must execute"),
    };
    let terminal = retirement_terminal(&admission);
    persist_terminal_blocking(&journal_path, &admission, terminal.clone())
        .expect("persist exact retirement Terminal");
    drop(claim);

    let sidecar_path = terminal_sidecar_path(&journal_path).expect("terminal sidecar path");
    let ledger_path =
        tracedecay_automation_runtime::automation::run_ledger::run_ledger_path(&dashboard_root);
    let journal_bytes = std::fs::read(&journal_path).expect("terminal journal bytes");
    let sidecar_bytes = std::fs::read(&sidecar_path).expect("terminal sidecar bytes");
    let ledger_bytes = std::fs::read(&ledger_path).expect("anchor ledger bytes");
    assert_eq!(
        recovery_index::indexed_journals_blocking(&dashboard_root, &admission.scope)
            .expect("pending retirement index")
            .len(),
        1
    );
    assert!(!archive_path.exists());

    let corrupt_source = b"source changed after exact retirement admission";
    write_private_test_file(&source_path, corrupt_source);
    let failed = recovery_composition::reconcile_reserved_automation_effects_for_project(
        &cg,
        &dashboard_root,
        &tracedecay_contracts::CancellationSignal::active(
            "cancellation.terminal-retirement-failure",
        )
        .expect("failure cancellation"),
    )
    .await
    .expect("retirement finalization failure is deferred");
    assert_eq!(failed.inspected, 1);
    assert_eq!(failed.deferred, 1);
    assert_eq!(
        std::fs::read(&source_path).expect("retained source"),
        corrupt_source.to_vec()
    );
    assert!(!archive_path.exists());
    assert_eq!(
        recovery_index::indexed_journals_blocking(&dashboard_root, &admission.scope)
            .expect("retained pending retirement")
            .len(),
        1
    );
    assert_eq!(
        std::fs::read(&journal_path).expect("unchanged journal"),
        journal_bytes
    );
    assert_eq!(
        std::fs::read(&sidecar_path).expect("unchanged sidecar"),
        sidecar_bytes
    );
    assert_eq!(
        std::fs::read(&ledger_path).expect("unchanged ledger"),
        ledger_bytes
    );

    write_private_test_file(&source_path, &source_bytes);
    let pending_retirement =
        tracedecay_automation_runtime::automation::effect_runtime::retirement::finalize_after_terminal(&dashboard_root, &plan.binding, Some(&plan))
            .expect("finalize exact retirement through source capture");
    assert!(!source_path.exists());
    assert_eq!(retirement_capture_count(&dashboard_root), 1);

    std::fs::create_dir(&source_path).expect("nonregular replacement source");
    recovery_index::remove_pending_for_retirement_blocking(
        &dashboard_root,
        &journal_path,
        &admission,
        &pending_retirement,
    )
    .expect("publish retirement transition before pending removal");
    assert_eq!(retirement_capture_count(&dashboard_root), 1);
    assert!(
        recovery_index::indexed_journals_blocking(&dashboard_root, &admission.scope)
            .expect("retirement transition removes pending entry")
            .is_empty()
    );
    tracedecay_automation_runtime::automation::effect_runtime::retirement::complete_after_pending_removal(&pending_retirement)
        .expect("complete retirement witness after pending removal");
    assert_eq!(retirement_capture_count(&dashboard_root), 0);
    recovery_index::finish_retirement_transition_blocking(
        &dashboard_root,
        &journal_path,
        &admission,
        &pending_retirement,
    )
    .expect("close durable retirement transition");
    recovery_index::reject_unbound_retirement_witness_if_index_empty(&dashboard_root)
        .expect("completed retirement leaves no unbound witness");
    assert!(source_path.is_dir());
    assert_eq!(retirement_capture_count(&dashboard_root), 0);
    assert!(
        recovery_index::indexed_journals_blocking(&dashboard_root, &admission.scope)
            .expect("pending-witness index closed")
            .is_empty()
    );

    std::fs::remove_dir(&source_path).expect("remove nonregular replacement fixture");
    write_private_test_file(&source_path, &source_bytes);
    recovery_index::add_pending_blocking(&dashboard_root, &journal_path, &admission)
        .expect("re-index Terminal before pending-absent crash");
    let orphaned_retirement =
        tracedecay_automation_runtime::automation::effect_runtime::retirement::finalize_after_terminal(&dashboard_root, &plan.binding, Some(&plan))
            .expect("capture exact source before pending-absent crash");
    assert!(!source_path.exists());
    assert_eq!(retirement_capture_count(&dashboard_root), 1);
    recovery_index::remove_pending_for_retirement_blocking(
        &dashboard_root,
        &journal_path,
        &admission,
        &orphaned_retirement,
    )
    .expect("durably hand off pending recovery before simulated crash");
    drop(orphaned_retirement);
    assert!(
        recovery_index::indexed_journals_blocking(&dashboard_root, &admission.scope)
            .expect("crash-state pending index")
            .is_empty()
    );
    let replacement_source =
        br#"{"schema_version":1,"proposals":[{"state":"pending_approval"}]}"#.to_vec();
    write_private_test_file(&source_path, &replacement_source);
    let pending_index_path = dashboard_root
        .join("automation_effects")
        .join("pending-index.json");
    let exact_transition_index = std::fs::read(&pending_index_path).expect("transition index");
    let mut mismatched_transition: serde_json::Value =
        serde_json::from_slice(&exact_transition_index).expect("transition index JSON");
    mismatched_transition["retirement_transitions"][0]["source_digest"] =
        serde_json::Value::String(format!("sha256:{}", "f".repeat(64)));
    write_private_test_file(
        &pending_index_path,
        &serde_json::to_vec_pretty(&mismatched_transition).expect("mismatched transition bytes"),
    );
    cg.close();
    let reopened = crate::tracedecay::TraceDecay::init_with_options(
        &project_root,
        crate::tracedecay::TraceDecayOpenOptions {
            profile_root: Some(profile_root.clone()),
            global_db_path: Some(profile_root.join("global.db")),
        },
    )
    .await
    .expect("reopen retirement recovery project");
    let rejected = recovery_composition::reconcile_reserved_automation_effects_for_project(
        &reopened,
        &dashboard_root,
        &tracedecay_contracts::CancellationSignal::active(
            "cancellation.terminal-retirement-mismatch",
        )
        .expect("mismatch cancellation"),
    )
    .await
    .expect("mismatched transition remains deferred");
    assert_eq!(rejected.inspected, 1);
    assert_eq!(rejected.deferred, 1);
    assert_eq!(retirement_capture_count(&dashboard_root), 1);
    assert_eq!(
        std::fs::read(&source_path).expect("replacement source retained across mismatch"),
        replacement_source
    );

    write_private_test_file(&pending_index_path, &exact_transition_index);
    let recovered = recovery_composition::reconcile_reserved_automation_effects_for_project(
        &reopened,
        &dashboard_root,
        &tracedecay_contracts::CancellationSignal::active(
            "cancellation.terminal-retirement-recovery",
        )
        .expect("recovery cancellation"),
    )
    .await
    .expect("recover exact retirement Terminal");
    assert_eq!(recovered.inspected, 1);
    assert_eq!(recovered.already_terminal, 1);
    assert_eq!(
        std::fs::read(&archive_path).expect("retirement archive"),
        source_bytes
    );
    assert_eq!(
        std::fs::read(&source_path).expect("replacement source preserved"),
        replacement_source
    );
    assert_eq!(retirement_capture_count(&dashboard_root), 0);
    assert!(
        recovery_index::indexed_journals_blocking(&dashboard_root, &admission.scope)
            .expect("closed retirement index")
            .is_empty()
    );

    write_private_test_file(&source_path, &source_bytes);
    recovery_index::add_pending_blocking(&dashboard_root, &journal_path, &admission)
        .expect("re-index Terminal before entry-plus-marker restart");
    let entry_plus_marker_retirement =
        tracedecay_automation_runtime::automation::effect_runtime::retirement::finalize_after_terminal(&dashboard_root, &plan.binding, Some(&plan))
            .expect("capture exact source before entry-plus-marker restart");
    recovery_index::remove_pending_for_retirement_blocking(
        &dashboard_root,
        &journal_path,
        &admission,
        &entry_plus_marker_retirement,
    )
    .expect("publish exact transition before entry-plus-marker restart");
    recovery_index::add_pending_blocking(&dashboard_root, &journal_path, &admission)
        .expect("simulate crash-visible entry plus exact marker");
    drop(entry_plus_marker_retirement);
    write_private_test_file(&source_path, &replacement_source);

    let entry_plus_marker =
        recovery_composition::reconcile_reserved_automation_effects_for_project(
            &reopened,
            &dashboard_root,
            &tracedecay_contracts::CancellationSignal::active(
                "cancellation.terminal-retirement-entry-plus-marker",
            )
            .expect("entry-plus-marker cancellation"),
        )
        .await
        .expect("entry-plus-marker restart converges through its marker first");
    assert_eq!(entry_plus_marker.inspected, 1);
    assert_eq!(entry_plus_marker.already_terminal, 1);
    assert_eq!(retirement_capture_count(&dashboard_root), 0);
    assert_eq!(
        std::fs::read(&source_path).expect("entry-plus-marker replacement preserved"),
        replacement_source
    );
    assert!(
        recovery_index::indexed_journals_blocking(&dashboard_root, &admission.scope)
            .expect("entry-plus-marker index closed")
            .is_empty()
    );
    assert_eq!(
        std::fs::read(&journal_path).expect("exact journal"),
        journal_bytes
    );
    assert_eq!(
        std::fs::read(&sidecar_path).expect("exact sidecar"),
        sidecar_bytes
    );
    assert_eq!(
        std::fs::read(&ledger_path).expect("exact ledger"),
        ledger_bytes
    );
    assert_eq!(
        read_indexed_terminal_blocking(&journal_path).expect("exact terminal readback"),
        Some(terminal.clone())
    );
    assert_eq!(
        tracedecay_automation_runtime::automation::run_ledger::find_run_record_exact_bounded_blocking(
            &dashboard_root,
            &anchor.ledger_record.run_id,
        )
        .expect("exact anchor lookup"),
        Some(anchor.ledger_record)
    );

    recovery_index::add_pending_blocking(&dashboard_root, &journal_path, &admission)
        .expect("re-index exact Terminal for idempotent retry");
    let replayed = recovery_composition::reconcile_reserved_automation_effects_for_project(
        &reopened,
        &dashboard_root,
        &tracedecay_contracts::CancellationSignal::active(
            "cancellation.terminal-retirement-idempotent",
        )
        .expect("idempotent cancellation"),
    )
    .await
    .expect("idempotently replay retirement finalization");
    assert_eq!(replayed.inspected, 1);
    assert_eq!(replayed.already_terminal, 1);
    assert_eq!(
        std::fs::read(&archive_path).expect("stable archive"),
        source_bytes
    );
    assert_eq!(
        std::fs::read(&source_path).expect("stable replacement source"),
        replacement_source
    );
    assert!(
        recovery_index::indexed_journals_blocking(&dashboard_root, &admission.scope)
            .expect("idempotently closed index")
            .is_empty()
    );
    assert_eq!(
        std::fs::read(&journal_path).expect("stable journal"),
        journal_bytes
    );
    assert_eq!(
        std::fs::read(&sidecar_path).expect("stable sidecar"),
        sidecar_bytes
    );
    assert_eq!(
        std::fs::read(&ledger_path).expect("stable ledger"),
        ledger_bytes
    );

    write_private_test_file(&source_path, &source_bytes);
    recovery_index::add_pending_blocking(&dashboard_root, &journal_path, &admission)
        .expect("re-index exact Terminal with archive and live admitted source");
    let archive_and_live_source =
        recovery_composition::reconcile_reserved_automation_effects_for_project(
            &reopened,
            &dashboard_root,
            &tracedecay_contracts::CancellationSignal::active(
                "cancellation.terminal-retirement-archive-live-source",
            )
            .expect("archive-live-source cancellation"),
        )
        .await
        .expect("project recovery retires an exact live source despite an existing archive");
    assert_eq!(archive_and_live_source.inspected, 1);
    assert_eq!(archive_and_live_source.already_terminal, 1);
    assert!(!source_path.exists());
    assert_eq!(
        std::fs::read(&archive_path).expect("archive remains exact"),
        source_bytes
    );
    assert!(
        recovery_index::indexed_journals_blocking(&dashboard_root, &admission.scope)
            .expect("archive-live-source index closed")
            .is_empty()
    );
    assert_eq!(
        std::fs::read(&journal_path).expect("archive-live-source journal"),
        journal_bytes
    );
    assert_eq!(
        std::fs::read(&sidecar_path).expect("archive-live-source sidecar"),
        sidecar_bytes
    );
    assert_eq!(
        std::fs::read(&ledger_path).expect("archive-live-source ledger"),
        ledger_bytes
    );
}

#[cfg(unix)]
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
        dashboard_root,
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
        dashboard_root,
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
async fn project_open_repairs_corrupt_append_intent_at_clean_eof_without_pending_journals() {
    let temp = tempfile::tempdir().expect("tempdir");
    let cg = retained_recovery_project(&temp, "clean-eof-corrupt-intent").await;
    let dashboard_root = &cg.store_layout().dashboard_root;
    let intent_path = dashboard_root.join("automation_runs.jsonl.append-intent");
    let corrupt = b"corrupt-clean-eof-intent";
    write_private_test_file(&intent_path, corrupt);

    let report = recovery_composition::reconcile_reserved_automation_effects_for_project(
        &cg,
        dashboard_root,
        &tracedecay_contracts::CancellationSignal::active("cancellation.clean-eof-corrupt-intent")
            .expect("recovery cancellation"),
    )
    .await
    .expect("project-open corrupt-intent repair");
    assert_eq!(report.inspected, 0);
    assert!(!intent_path.exists());
    assert_eq!(
        std::fs::read_dir(dashboard_root.join("automation_run_append_intent_quarantine"))
            .expect("corrupt-intent quarantine")
            .filter_map(std::result::Result::ok)
            .map(|entry| std::fs::read(entry.path()).expect("quarantined intent"))
            .collect::<Vec<_>>(),
        vec![corrupt.to_vec()]
    );
    assert!(
        recovery_index::indexed_journals_blocking(
            dashboard_root,
            &tracedecay_code_index_runtime::resolved_scope_for_project(
                cg.project_root(),
                &match cg.project_memory_owner().expect("project owner") {
                    FactOwnerV1::Project { project_id } => project_id,
                    FactOwnerV1::Profile => panic!("recovery fixture requires a project owner"),
                },
            )
            .expect("project scope"),
        )
        .expect("empty pending index")
        .is_empty()
    );
}

#[tokio::test]
async fn recovery_defers_unavailable_memory_without_blocking_external_or_terminal_effects() {
    let temp = tempfile::tempdir().expect("tempdir");
    let fixture_name = "corrupt-intent-before-memory-open";
    let project_root = temp.path().join(format!("{fixture_name}-project"));
    let profile_root = temp.path().join(format!("{fixture_name}-profile"));
    let cg = retained_recovery_project(&temp, fixture_name).await;
    let dashboard_root = cg.store_layout().dashboard_root.clone();
    let admission = admission_for_recovery_project(
        &cg,
        "run.corrupt-intent-before-memory-open",
        "request.corrupt-intent-before-memory-open",
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
    let intent_path = dashboard_root.join("automation_runs.jsonl.append-intent");
    let corrupt = b"corrupt-before-memory-open";
    write_private_test_file(&intent_path, corrupt);
    cg.close();

    let read_only = crate::tracedecay::TraceDecay::open_read_only_with_options(
        &project_root,
        crate::tracedecay::TraceDecayOpenOptions {
            profile_root: Some(profile_root.clone()),
            global_db_path: Some(profile_root.join("global.db")),
        },
    )
    .await
    .expect("open read-only recovery project");
    let report = recovery_composition::reconcile_reserved_automation_effects_for_project(
        &read_only,
        &dashboard_root,
        &tracedecay_contracts::CancellationSignal::active(
            "cancellation.corrupt-intent-before-memory-open",
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
    assert!(!intent_path.exists());
    assert_eq!(
        std::fs::read_dir(dashboard_root.join("automation_run_append_intent_quarantine"))
            .expect("corrupt-intent quarantine")
            .filter_map(std::result::Result::ok)
            .map(|entry| std::fs::read(entry.path()).expect("quarantined intent"))
            .collect::<Vec<_>>(),
        vec![corrupt.to_vec()]
    );
}

#[tokio::test]
async fn empty_pending_index_does_not_open_project_memory() {
    let temp = tempfile::tempdir().expect("tempdir");
    let fixture_name = "empty-index-with-unavailable-memory";
    let project_root = temp.path().join(format!("{fixture_name}-project"));
    let profile_root = temp.path().join(format!("{fixture_name}-profile"));
    let cg = retained_recovery_project(&temp, fixture_name).await;
    let dashboard_root = cg.store_layout().dashboard_root.clone();
    cg.close();

    let read_only = crate::tracedecay::TraceDecay::open_read_only_with_options(
        &project_root,
        crate::tracedecay::TraceDecayOpenOptions {
            profile_root: Some(profile_root.clone()),
            global_db_path: Some(profile_root.join("global.db")),
        },
    )
    .await
    .expect("open read-only recovery project");
    let report = recovery_composition::reconcile_reserved_automation_effects_for_project(
        &read_only,
        &dashboard_root,
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

#[tokio::test]
async fn project_open_truncates_unique_spool_partial_with_empty_pending_index() {
    let temp = tempfile::tempdir().expect("tempdir");
    let cg = retained_recovery_project(&temp, "partial-corrupt-intent").await;
    let dashboard_root = &cg.store_layout().dashboard_root;
    let run_id = "run.project-open-corrupt-intent";
    let (run, guard) =
        retained_disabled_user_job(dashboard_root, run_id, "project-open-corrupt-intent").await;
    drop(guard);
    let expected_record = run.ledger_record;
    let (publication, _) =
        tracedecay_automation_runtime::automation::run_ledger::bind_staged_run_record_exact(
            dashboard_root,
            &expected_record,
            |publication| Ok(publication.clone()),
        )
        .expect("stage exact recovery spool");
    let spool_files = exact_spool_files(dashboard_root);
    assert_eq!(spool_files.len(), 1);
    let spool = std::fs::read(&spool_files[0]).expect("exact spool payload");
    let ledger_path =
        tracedecay_automation_runtime::automation::run_ledger::run_ledger_path(dashboard_root);
    std::fs::write(&ledger_path, &spool[..spool.len() / 2]).expect("owned partial ledger tail");
    let intent_path = dashboard_root.join("automation_runs.jsonl.append-intent");
    std::fs::write(&intent_path, b"corrupt-partial-intent").expect("corrupt append intent");

    let report = recovery_composition::reconcile_reserved_automation_effects_for_project(
        &cg,
        dashboard_root,
        &tracedecay_contracts::CancellationSignal::active("cancellation.partial-corrupt-intent")
            .expect("recovery cancellation"),
    )
    .await
    .expect("project-open partial-intent repair");
    assert_eq!(report.inspected, 0);
    assert!(!intent_path.exists());
    assert_eq!(
        std::fs::metadata(&ledger_path)
            .expect("repaired ledger")
            .len(),
        0
    );
    assert_eq!(exact_spool_file_count(dashboard_root), 1);

    assert_eq!(
        tracedecay_automation_runtime::automation::run_ledger::publish_staged_run_record_exact(
            dashboard_root,
            run_id,
            &publication,
        )
        .await
        .expect("publish repaired exact row"),
        tracedecay_automation_runtime::automation::run_ledger::ExactRunPublishOutcome::Published
    );
    assert_eq!(
        tracedecay_automation_runtime::automation::run_ledger::find_run_record_exact_bounded_blocking(
            dashboard_root,
            run_id,
        )
        .expect("exact repaired row"),
        Some(expected_record)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reused_scheduler_skip_abandons_current_effect_before_observing_exact_prior() {
    use fs2::FileExt;
    use std::sync::Arc;
    use std::time::Duration;
    use tracedecay_automation_runtime::automation::AutomationRunControl;
    use tracedecay_automation_runtime::automation::backend::{AgentTaskKind, task_key};
    use tracedecay_automation_runtime::automation::config::{
        AutomationBackend, AutomationConfig, AutomationHostMode,
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
    let cg = crate::tracedecay::TraceDecay::init_with_options(
        &project_root,
        crate::tracedecay::TraceDecayOpenOptions {
            profile_root: Some(profile_root.clone()),
            global_db_path: Some(profile_root.join("global.db")),
        },
    )
    .await
    .expect("initialize fixed-task automation project");
    let project_context = cg
        .automation_project_context()
        .expect("compose project automation context");
    let dashboard_root = &cg.store_layout().dashboard_root;
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::Disabled,
        host_mode: AutomationHostMode::Standalone,
        ..AutomationConfig::default()
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
        dashboard_root,
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

    let ledger_path =
        tracedecay_automation_runtime::automation::run_ledger::run_ledger_path(dashboard_root);
    let prior_ledger_bytes = std::fs::read(&ledger_path).expect("prior exact ledger bytes");
    assert_eq!(
        tracedecay_automation_runtime::automation::run_ledger::find_run_record_exact_bounded_blocking(
            dashboard_root,
            prior_run_id,
        )
        .expect("prior exact lookup"),
        Some(prior_record.clone())
    );
    assert_eq!(prior_record.task, AgentTaskKind::MemoryCurator);
    assert_eq!(
        prior_record.task_key.as_deref(),
        Some(task_key(AgentTaskKind::MemoryCurator))
    );
    assert_eq!(prior_record.error.as_deref(), Some("backend_disabled"));
    let prior_spool_files = exact_spool_files(dashboard_root);

    let wrong_task_run_id = "run.reused-scheduler-skip.wrong-task";
    let (mut wrong_task_reused, wrong_task_guard) =
        retained_repeated_memory_curator(&cg, &config, &configuration_revision, wrong_task_run_id)
            .await;
    wrong_task_reused.task_key = task_key(AgentTaskKind::SessionReflector).to_owned();
    let (wrong_task_authority, wrong_task_journal, wrong_task_admission) =
        retained_external_authority(
            dashboard_root,
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
        std::fs::read(&ledger_path).expect("ledger after wrong-task rejection"),
        prior_ledger_bytes
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
            dashboard_root,
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
        std::fs::read(&ledger_path).expect("ledger after wrong-reason rejection"),
        prior_ledger_bytes
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
        dashboard_root,
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
    journal_lock.lock_exclusive().expect("block abandonment");
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
        tracedecay_automation_runtime::automation::run_ledger::find_run_record_exact_bounded_blocking(
            dashboard_root,
            current_run_id,
        )
        .expect("current exact absence")
        .is_none()
    );
    assert!(fixed_task_lock_is_denied(dashboard_root, AgentTaskKind::MemoryCurator).await);

    FileExt::unlock(&journal_lock).expect("release abandonment");
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
        std::fs::read(&ledger_path).expect("ledger after abandonment"),
        prior_ledger_bytes,
        "reusing a scheduler skip must not append a current physical row"
    );
    assert!(
        tracedecay_automation_runtime::automation::run_ledger::find_run_record_exact_bounded_blocking(
            dashboard_root,
            current_run_id,
        )
        .expect("current exact absence after abandonment")
        .is_none()
    );
    assert_eq!(exact_spool_files(dashboard_root), prior_spool_files);
    assert!(
        tracedecay_automation_runtime::automation::scheduler::AutomationTaskLock::try_acquire(
            dashboard_root,
            AgentTaskKind::MemoryCurator,
            None,
            now_secs(),
        )
        .await
        .expect("post-abandonment task lock")
        .is_some()
    );
}
