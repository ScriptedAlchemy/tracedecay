use super::*;

use serde_json::json;
use std::collections::BTreeMap;
use tracedecay_agent_hosts::automation::AutomationCommittedReceipt;
use tracedecay_agent_hosts::automation::run_ledger::AutomationRunLedgerRecord;
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
    EffectId, EffectReceipt, EffectResult, EffectTermination, IdempotencyKey, OperationBudgetUsage,
    OperationReceipt, PolicyDecisionRef, ReconciliationState, RequestId, ResolvedScope,
};
use tracedecay_domain::{
    ActorId, ComponentVersion, FactId, FactIdentityMaterialV1, FactIdentitySourceV1,
    ManifestDigest, ProjectId, ProvenanceId, RepositoryId, RunId, UtcMicros, WorktreeId,
    canonical_sha256,
};
use tracedecay_tool_catalog::EffectClass;

use crate::daemon::automation_effect::{AutomationSettledTerminal, recovery_index};

struct NeverAutomationBackend;

impl tracedecay_agent_hosts::automation::backend::AgentTaskBackend for NeverAutomationBackend {
    fn run_task(
        &self,
        _request: &tracedecay_agent_hosts::automation::backend::AgentTaskRequest,
    ) -> std::result::Result<
        tracedecay_agent_hosts::automation::backend::AgentTaskResponse,
        tracedecay_agent_hosts::automation::backend::AgentTaskError,
    > {
        panic!("disabled retained automation must not invoke its backend")
    }
}

fn digest(seed: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", seed.to_string().repeat(64))).expect("fixture digest")
}

fn exact_publication(seed: char, payload_len: u64) -> ExactRunPublication {
    serde_json::from_value(json!({
        "schema_version": 1,
        "ledger_digest": format!("sha256:{}", seed.to_string().repeat(64)),
        "payload_len": payload_len,
    }))
    .expect("exact publication")
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
    admission.effect_authority_digest = super::super::recovery_index::effect_authority_digest(
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
    })
}

fn external_admission(run_id: &str, request_id: &str) -> DurableAutomationAdmission {
    external_admission_for_job(run_id, request_id, "nightly-delivery")
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
    let recovery_scope = crate::daemon::project_open_owners::resolved_scope_for_project(
        cg.project_root(),
        &project_id,
    )
    .expect("recovery scope");
    let mut admission = external_admission_for_job(run_id, request_id, job_id);
    admission.scope = recovery_scope.clone();
    admission.effect_receipt_template.scope = recovery_scope.clone();
    admission.recovery = AutomationRecoveryBinding::External {
        recovery_problem: reset_problem(&admission.request_id, &recovery_scope, &admission.request),
    };
    seal_effect_authority(admission)
}

fn retirement_admission_for_recovery_project(
    cg: &crate::tracedecay::TraceDecay,
    run_id: &str,
    request_id: &str,
    binding: super::super::retirement::RetirementBinding,
) -> DurableAutomationAdmission {
    let owner = cg.project_memory_owner().expect("project memory owner");
    let FactOwnerV1::Project { project_id } = owner.clone() else {
        panic!("automation retirement fixture requires a project owner")
    };
    let recovery_scope = crate::daemon::project_open_owners::resolved_scope_for_project(
        cg.project_root(),
        &project_id,
    )
    .expect("retirement recovery scope");
    let mut admission = admission(run_id, request_id);
    admission.request.task = AutomationTaskRequestV1::SessionReflector(
        tracedecay_application::retained_surfaces::SessionReflectorRunInputV1 {
            provider: "cursor".to_owned(),
            query: "retire exact shipped proposal history".to_owned(),
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

fn retained_external_authority(
    dashboard_root: &std::path::Path,
    admission: DurableAutomationAdmission,
) -> (
    super::super::AutomationEffectAuthority,
    std::path::PathBuf,
    DurableAutomationAdmission,
) {
    use std::collections::BTreeSet;

    use tracedecay_application::{
        CancellationContext, CancellationSignal, CapabilityGrantSnapshot, RequestContext,
        RetainedSurfaceExecutionContextV1,
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
    let execution = RetainedSurfaceExecutionContextV1 {
        request_context: &context,
        cancellation_signal: &cancellation,
        operation: &operation,
        observed_at: UtcMicros(2),
    };
    let prepared = super::super::prepare_retained_effect(
        &execution,
        RetainedSurfaceOperation::FactStoreCurate,
        &admission.configuration_digest,
        &admission.request,
        admission.request.run_id.as_str(),
    )
    .expect("prepared retained effect");
    let placeholder = digest('f');
    let RetainedSurfaceExecutionErrorV1::PartialEffect {
        mut committed_receipt,
        ..
    } = prepared.partial_error_with_digest(
        &placeholder,
        "application.automation-run.recovery-template",
        "Durable automation recovery receipt template.",
    )
    else {
        panic!("prepared effect must construct a recovery template")
    };
    committed_receipt.committed_state = None;
    assert_ne!(
        admission.input_digest, committed_receipt.input_digest,
        "automation-run admission identity and retained-effect receipt identity are distinct domains"
    );
    let mut admission = admission;
    admission.effect_receipt_template = *committed_receipt;
    let admission = seal_effect_authority(admission);
    let journal_path = canonical_journal_path(dashboard_root, &admission.request.run_id);
    let claim = match reserve_or_replay_blocking(&journal_path, admission.clone())
        .expect("durable admission")
    {
        ReservationResult::Execute { claim, .. } => claim,
        _ => panic!("fresh retained fixture must execute"),
    };
    let expected_admission = admission.clone();
    (
        super::super::AutomationEffectAuthority {
            context,
            cancellation,
            operation,
            prepared,
            admission,
            journal_path: journal_path.clone(),
            dashboard_root: dashboard_root.to_path_buf(),
            _reservation_claim: Some(claim),
        },
        journal_path,
        expected_admission,
    )
}

async fn retained_disabled_user_job(
    dashboard_root: &std::path::Path,
    run_id: &str,
    job_id: &str,
) -> (
    tracedecay_agent_hosts::automation::jobs::UserJobAutomationRun,
    tracedecay_agent_hosts::automation::runner::AutomationRunSettlementGuard,
) {
    let retained = retained_disabled_user_job_run(dashboard_root, run_id, job_id).await;
    let (result, guard) = retained.into_parts();
    (result.expect("disabled retained job terminal"), guard)
}

async fn retained_disabled_user_job_run(
    dashboard_root: &std::path::Path,
    run_id: &str,
    job_id: &str,
) -> tracedecay_agent_hosts::automation::runner::RetainedAutomationRun<
    tracedecay_agent_hosts::automation::jobs::UserJobAutomationRun,
> {
    use tracedecay_agent_hosts::automation::config::{
        AutomationBackend, AutomationConfig, AutomationHostMode,
    };
    use tracedecay_agent_hosts::automation::jobs::{
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

async fn task_lock_is_denied(dashboard_root: &std::path::Path, job_id: &str) -> bool {
    tracedecay_agent_hosts::automation::scheduler::AutomationTaskLock::try_acquire_keyed(
        dashboard_root,
        &format!("user_job_{job_id}"),
        None,
        now_secs(),
    )
    .await
    .expect("competing task lock")
    .is_none()
}

async fn fixed_task_lock_is_denied(
    dashboard_root: &std::path::Path,
    task: tracedecay_agent_hosts::automation::backend::AgentTaskKind,
) -> bool {
    tracedecay_agent_hosts::automation::scheduler::AutomationTaskLock::try_acquire(
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
    config: &tracedecay_agent_hosts::automation::config::AutomationConfig,
    configuration_revision: &tracedecay_domain::configuration::ConfigurationRevisionId,
    run_id: &str,
) -> (
    tracedecay_agent_hosts::automation::runner::ReusedSchedulerSkip,
    tracedecay_agent_hosts::automation::runner::AutomationRunSettlementGuard,
) {
    use tracedecay_agent_hosts::automation::runner::RetainedAutomationSettlementDisposition;

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
    config: &tracedecay_agent_hosts::automation::config::AutomationConfig,
    configuration_revision: &tracedecay_domain::configuration::ConfigurationRevisionId,
    run_id: &str,
) -> tracedecay_agent_hosts::automation::runner::RetainedAutomationRun<
    tracedecay_agent_hosts::automation::runner::MemoryCuratorAutomationRun,
> {
    use std::sync::Arc;
    use tracedecay_agent_hosts::automation::AutomationRunControl;
    use tracedecay_agent_hosts::automation::run_ledger::AutomationTrigger;
    use tracedecay_agent_hosts::automation::runner::{
        MemoryCuratorAutomationOptions, run_memory_curator_with_backend_for_retained_settlement,
    };

    let run_control = AutomationRunControl::from_interrupted(Arc::new(|| false));
    run_memory_curator_with_backend_for_retained_settlement(
        cg,
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

fn assert_admission_conflict(result: crate::errors::Result<ReservationResult>) {
    assert!(matches!(
        result.expect("valid durable mismatch"),
        ReservationResult::Conflict { .. }
    ));
}

fn assert_effect_authority(admission: &DurableAutomationAdmission, expected: bool, label: &str) {
    let operation =
        retained_surface_application_operation(RetainedSurfaceOperation::FactStoreCurate)
            .expect("operation");
    assert_eq!(
        super::super::recovery_index::admission_has_exact_authority(admission, &operation)
            .expect("authority classification"),
        expected,
        "{label}"
    );
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
            reason: tracedecay_application::retained_surfaces::AutomationSkipReasonV1::from_ledger_reason(
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

fn partial_terminal(admission: &DurableAutomationAdmission) -> AutomationSettledTerminal {
    let receipt_run_id = admission.request.run_id.as_str();
    let owner = FactOwnerV1::Project {
        project_id: admission.scope.project_id.clone(),
    };
    let fact_id = FactId::derive(
        &FactIdentityMaterialV1::new(
            owner.clone(),
            FactIdentitySourceV1::Application {
                operation_id: ProvenanceId::new("operation.memory-journal").expect("operation id"),
            },
        )
        .expect("identity material"),
    )
    .expect("owner-bound fact id");
    let mut receipt: MemoryAutomationCurationReceiptV1 = serde_json::from_value(json!({
            "receipt": {
                "owner": owner,
                "operation_id":"operation.memory-journal",
                "input_digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "automation_run_id": receipt_run_id,
                "operation_effects":[{"kind":"normalize_tags","fact_id":fact_id,"commit":{"disposition":"committed","fact_id":fact_id,"owner":owner,"committed_event_ids":["event.memory-journal.fact","event.memory-journal.provenance"],"last_event_id":"event.memory-journal.provenance","active_assertion_id":"assertion.memory-journal"}}],
                "replay_fact_id":fact_id,"replay_event_id":"event.memory-journal.provenance","changed_fact_ids":[fact_id],
                "accepted_operations":1,"facts_added":0,"facts_updated":0,"facts_merged":0,"facts_removed":0,"normalized_tags":1,"facts_linked":0
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
    let operation =
        retained_surface_application_operation(RetainedSurfaceOperation::FactStoreCurate)
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
            committed_receipt: Box::new(effect_receipt),
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
fn durable_admission_accepts_distinct_run_and_retained_effect_input_digests() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("layered-input-digests.json");
    let admission = admission("run.layered-input-digests", "request.layered-input-digests");

    assert_ne!(
        admission.input_digest, admission.effect_receipt_template.input_digest,
        "the automation-run identity and retained-effect receipt use distinct domains"
    );
    let claim = match reserve_or_replay_blocking(&path, admission.clone())
        .expect("fresh layered admission")
    {
        ReservationResult::Execute { claim, .. } => claim,
        _ => panic!("fresh layered admission must execute"),
    };
    let stored = read_indexed_record_blocking(&path)
        .expect("layered admission read")
        .expect("layered admission record");
    assert_eq!(stored.admission(), &admission);
    assert_ne!(
        stored.admission().input_digest,
        stored.admission().effect_receipt_template.input_digest
    );
    drop(claim);
}

#[test]
fn legacy_terminal_wire_shape_migrates_without_losing_replay() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("legacy-terminal.json");
    let admitted = admission("run.legacy-journal", "request.legacy-journal");
    let terminal = success_terminal(&admitted, "run.legacy-journal");
    let legacy = json!({
        "admission": admitted,
        "state": {
            "state": "terminal",
            "terminal": terminal,
        },
    });
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&legacy).expect("legacy bytes"),
    )
    .expect("legacy journal");

    let requested = admission("run.legacy-journal", "request.legacy-journal");
    let ReservationResult::Replay {
        terminal: replayed, ..
    } = reserve_or_replay_blocking(&path, requested).expect("legacy replay")
    else {
        panic!("legacy terminal must replay")
    };
    assert_eq!(replayed, terminal);
    let migrated: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("migrated bytes"))
            .expect("migrated journal");
    assert_eq!(migrated["state"]["state"], "terminal");
    assert!(migrated["state"].get("terminal").is_none());
    assert_eq!(migrated["state"]["value"]["terminal"]["schema_version"], 1);
    assert!(terminal_sidecar_path(&path).expect("sidecar").exists());
}

#[test]
fn invalid_fresh_admission_is_rejected_before_any_durable_write() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("invalid-fresh.json");
    let mut invalid = admission("run.invalid-journal", "request.invalid-journal");
    invalid.schema_version = 2;

    let mut ensure_pending_called = false;
    let mut rollback_pending_called = false;
    assert!(
        reserve_or_replay_indexed_blocking(
            &path,
            invalid,
            || {
                ensure_pending_called = true;
                Ok(())
            },
            || {
                rollback_pending_called = true;
                Ok(())
            },
        )
        .is_err()
    );
    assert!(!ensure_pending_called);
    assert!(!rollback_pending_called);
    assert!(!path.exists());
    assert!(!terminal_sidecar_path(&path).expect("sidecar").exists());
}

#[test]
fn invalid_requested_schema_cannot_downgrade_an_existing_reservation_to_conflict() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("invalid-requested-schema.json");
    let admitted = admission(
        "run.invalid-requested-schema",
        "request.invalid-requested-schema",
    );
    reserve_or_replay_blocking(&path, admitted.clone()).expect("reserve");
    let before = std::fs::read(&path).expect("reserved bytes");
    let mut unsupported = admitted;
    unsupported.schema_version = 2;

    assert!(reserve_or_replay_blocking(&path, unsupported).is_err());
    assert_eq!(std::fs::read(&path).expect("preserved reservation"), before);
}

#[test]
fn reserved_read_removes_an_orphan_terminal_sidecar() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("orphan-terminal-sidecar.json");
    let admitted = admission("run.orphan-terminal", "request.orphan-terminal");
    reserve_or_replay_blocking(&path, admitted.clone()).expect("reserve");
    let sidecar = terminal_sidecar_path(&path).expect("sidecar path");
    write_terminal_sidecar(&path, &success_terminal(&admitted, "run.orphan-terminal"))
        .expect("simulate crash-partial sidecar");
    assert!(sidecar.exists());

    // The orphan sidecar is crash residue of an unbound terminal: the read
    // removes it and the abandoned reservation enters recovery instead of
    // replaying or re-executing.
    assert!(matches!(
        reserve_or_replay_blocking(&path, admitted).expect("orphan cleanup enters recovery"),
        ReservationResult::Recover { .. }
    ));
    assert!(!sidecar.exists());
}

#[test]
fn cancellation_does_not_suppress_an_already_durable_terminal_replay() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("cancelled-replay.json");
    let admission = admission("run.cancelled-replay", "request.cancelled-replay");
    reserve_or_replay_blocking(&path, admission.clone()).expect("reserve");
    let terminal = success_terminal(&admission, "run.cancelled-replay");
    persist_terminal_blocking(&path, &admission, terminal.clone()).expect("terminal");
    let cancellation =
        tracedecay_application::CancellationSignal::active("cancellation.durable-replay")
            .expect("cancellation");
    assert!(cancellation.cancel(UtcMicros(20)));

    assert_eq!(
        persist_recovered_terminal_blocking(
            &path,
            &admission,
            terminal.clone(),
            Some(&cancellation),
        )
        .expect("durable replay"),
        Some(terminal)
    );
}

#[test]
fn prepared_terminal_recovers_in_the_same_process_and_promotes_exactly() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("prepared-terminal.json");
    let admission = admission("run.prepared-journal", "request.prepared-journal");
    reserve_or_replay_blocking(&path, admission.clone()).expect("reserve");
    let terminal = success_terminal(&admission, "run.prepared-journal");
    let publication = exact_publication('e', 8_192);

    persist_prepared_terminal_blocking(&path, &admission, &terminal, publication.clone())
        .expect("persist prepared terminal");
    let ReservationResult::RecoverPrepared {
        terminal: recovered,
        publication: recovered_publication,
        ..
    } = reserve_or_replay_blocking(&path, admission.clone()).expect("recover prepared terminal")
    else {
        panic!("prepared state must recover even in the owning process")
    };
    assert_eq!(recovered, terminal);
    assert_eq!(recovered_publication, publication);

    let promoted =
        promote_prepared_terminal_blocking(&path, &admission, terminal.clone(), &publication)
            .expect("promote prepared terminal");
    assert_eq!(promoted, terminal);
    let ReservationResult::Replay {
        terminal: replayed,
        publication: Some(replayed_publication),
        ..
    } = reserve_or_replay_blocking(&path, admission).expect("replay promoted terminal")
    else {
        panic!("promoted terminal must replay its publication binding")
    };
    assert_eq!(replayed, terminal);
    assert_eq!(replayed_publication, publication);
}

#[test]
fn visible_sidecar_replace_error_is_republished_before_binding() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("visible-sidecar-replace.json");
    let admission = admission(
        "run.visible-sidecar-replace",
        "request.visible-sidecar-replace",
    );
    let terminal = success_terminal(&admission, "run.visible-sidecar-replace");

    let error =
        write_terminal_sidecar_with_publisher(&path, &terminal, |temporary, destination| {
            replace_automation_file_atomically(
                temporary,
                destination,
                "automation terminal sidecar",
            )?;
            Err(std::io::Error::other(
                "injected error after visible terminal-sidecar replacement",
            ))
        })
        .expect_err("visible sidecar uncertainty must surface");
    assert!(error.to_string().contains("visible terminal-sidecar"));
    let binding = terminal_binding(&terminal).expect("terminal binding");
    assert_eq!(
        read_terminal_sidecar_if_present(
            &terminal_sidecar_path(&path).expect("sidecar path"),
            &binding,
        )
        .expect("visible sidecar read"),
        Some(terminal.clone())
    );

    let republished = std::cell::Cell::new(false);
    let replayed =
        write_terminal_sidecar_with_publisher(&path, &terminal, |temporary, destination| {
            republished.set(true);
            replace_automation_file_atomically(
                temporary,
                destination,
                "automation terminal sidecar",
            )
        })
        .expect("retry republishes and reads back the exact sidecar");
    assert!(republished.get());
    assert_eq!(replayed, binding);
    assert_eq!(
        read_terminal_sidecar(&path, &binding).expect("durable sidecar replay"),
        terminal
    );
}

#[cfg(windows)]
#[test]
fn indexed_terminal_publishers_replace_a_held_journal_with_private_files() {
    use std::io::{Read, Seek};

    let temp = tempfile::tempdir().expect("tempdir");
    let dashboard_root = temp.path();
    let admission = external_admission(
        "run.windows-private-publishers",
        "request.windows-private-publishers",
    );
    let journal_path = canonical_journal_path(dashboard_root, &admission.request.run_id);
    let pending_index_path = dashboard_root
        .join("automation_effects")
        .join("pending-index.json");
    let claim = match reserve_or_replay_indexed_blocking(
        &journal_path,
        admission.clone(),
        || recovery_index::add_pending_blocking(dashboard_root, &journal_path, &admission),
        || recovery_index::remove_pending_blocking(dashboard_root, &journal_path),
    )
    .expect("indexed private reservation")
    {
        ReservationResult::Execute { claim, .. } => claim,
        _ => panic!("fresh indexed reservation must execute"),
    };
    tracedecay_runtime_core::windows_security::validate_private_file(&journal_path)
        .expect("private Reserved journal");
    tracedecay_runtime_core::windows_security::validate_private_file(&pending_index_path)
        .expect("private pending index");
    let reserved_bytes = std::fs::read(&journal_path).expect("Reserved journal bytes");
    let mut held_reader =
        tracedecay_runtime_core::windows_security::open_private_file(&journal_path)
            .expect("held Reserved journal reader");

    let terminal = AutomationSettledTerminal::Problem(admission.recovery_problem().clone());
    persist_terminal_blocking(&journal_path, &admission, terminal)
        .expect("publish private sidecar and replace held journal");

    held_reader.rewind().expect("rewind held Reserved reader");
    let mut held_bytes = Vec::new();
    held_reader
        .read_to_end(&mut held_bytes)
        .expect("read retained Reserved handle");
    assert_eq!(held_bytes, reserved_bytes);
    assert_ne!(
        std::fs::read(&journal_path).expect("Terminal journal bytes"),
        reserved_bytes
    );
    assert!(
        read_indexed_record_blocking(&journal_path)
            .expect("read Terminal journal")
            .expect("Terminal journal")
            .is_terminal()
    );
    tracedecay_runtime_core::windows_security::validate_private_file(&journal_path)
        .expect("private Terminal journal");
    tracedecay_runtime_core::windows_security::validate_private_file(
        &terminal_sidecar_path(&journal_path).expect("terminal sidecar path"),
    )
    .expect("private terminal sidecar");
    tracedecay_runtime_core::windows_security::validate_private_file(&pending_index_path)
        .expect("private retained pending index");
    drop(claim);
}

#[cfg(unix)]
fn assert_private_unix_stage_and_replace(
    temporary: &Path,
    destination: &Path,
    record_name: &str,
) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    assert_eq!(
        std::fs::symlink_metadata(temporary)?.permissions().mode() & 0o777,
        0o600,
        "{record_name} staging mode"
    );
    replace_automation_file_atomically(temporary, destination, record_name)
}

#[cfg(unix)]
#[test]
fn journal_sidecar_and_index_publishers_stage_mode_0600() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().expect("tempdir");
    let journal_path = temp.path().join("private-stage-journal.json");
    let admission = admission("run.private-stage", "request.private-stage");
    let record = DurableAutomationRecord {
        admission: admission.clone(),
        state: DurableAutomationState::Reserved,
        legacy_terminal: None,
    };
    write_record_with_publisher(&journal_path, &record, |temporary, destination| {
        assert_private_unix_stage_and_replace(temporary, destination, "automation terminal journal")
    })
    .expect("private-mode journal publication");

    let terminal = success_terminal(&admission, "run.private-stage");
    write_terminal_sidecar_with_publisher(&journal_path, &terminal, |temporary, destination| {
        assert_private_unix_stage_and_replace(temporary, destination, "automation terminal sidecar")
    })
    .expect("private-mode sidecar publication");

    let pending_index_path = temp
        .path()
        .join("automation_effects")
        .join("pending-index.json");
    std::fs::create_dir_all(
        pending_index_path
            .parent()
            .expect("pending index parent directory"),
    )
    .expect("pending index directory");
    let pending_index = serde_json::to_vec_pretty(&json!({
        "schema_version": 1,
        "entries": [],
    }))
    .expect("pending index bytes");
    recovery_index::write_pending_index_with_publisher(
        &pending_index_path,
        &pending_index,
        |temporary, destination| {
            assert_private_unix_stage_and_replace(
                temporary,
                destination,
                "automation pending recovery index",
            )
        },
    )
    .expect("private-mode pending index publication");

    for path in [
        journal_path.clone(),
        terminal_sidecar_path(&journal_path).expect("terminal sidecar path"),
        pending_index_path,
    ] {
        assert_eq!(
            std::fs::symlink_metadata(&path)
                .expect("published private file metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "published mode for '{}'",
            path.display()
        );
    }
}

#[tokio::test]
async fn visible_terminal_replace_error_after_exact_publish_retains_cleanup_authority() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dashboard_root = temp.path();
    let run_id = "run.visible-terminal-replace";
    let job_id = "visible-terminal-replace";
    let (run, guard) = retained_disabled_user_job(dashboard_root, run_id, job_id).await;
    let expected_record = run.ledger_record;
    let admission = external_admission_for_job(run_id, "request.visible-terminal-replace", job_id);
    let journal_path = canonical_journal_path(dashboard_root, &admission.request.run_id);
    recovery_index::add_pending_blocking(dashboard_root, &journal_path, &admission)
        .expect("pending settlement authority");
    let claim = match reserve_or_replay_blocking(&journal_path, admission.clone())
        .expect("durable reservation")
    {
        ReservationResult::Execute { claim, .. } => claim,
        _ => panic!("fresh settlement must execute"),
    };
    let terminal = AutomationSettledTerminal::Problem(admission.recovery_problem().clone());
    let (publication, ()) =
        tracedecay_agent_hosts::automation::run_ledger::bind_staged_run_record_exact(
            dashboard_root,
            &expected_record,
            |publication| {
                persist_prepared_terminal_blocking(
                    &journal_path,
                    &admission,
                    &terminal,
                    publication.clone(),
                )
            },
        )
        .expect("stage and prepare exact terminal");
    assert_eq!(
        tracedecay_agent_hosts::automation::run_ledger::publish_staged_run_record_exact_blocking(
            dashboard_root,
            run_id,
            &publication,
        )
        .expect("publish exact row"),
        tracedecay_agent_hosts::automation::run_ledger::ExactRunPublishOutcome::Published
    );
    assert_eq!(exact_spool_file_count(dashboard_root), 1);

    let error = promote_prepared_terminal_with_writer(
        &journal_path,
        &admission,
        terminal.clone(),
        &publication,
        |path, record| {
            write_record_with_publisher(path, record, |temporary, destination| {
                std::fs::remove_file(destination)?;
                std::fs::rename(temporary, destination)?;
                Err(std::io::Error::other(
                    "injected error after weak visible terminal-journal publication",
                ))
            })
        },
    )
    .expect_err("visible terminal replacement uncertainty must surface");
    assert!(error.to_string().contains("weak visible terminal-journal"));
    assert!(
        read_record(&journal_path)
            .expect("physical visible journal")
            .expect("visible terminal journal")
            .is_terminal()
    );
    assert_eq!(exact_spool_file_count(dashboard_root), 1);
    assert_eq!(
        recovery_index::indexed_journals_blocking(dashboard_root, &admission.scope)
            .expect("retained pending authority")
            .len(),
        1
    );

    let classifier_error = classify_durable_settlement_with_stabilizer(
        &journal_path,
        &admission,
        &terminal,
        Some(&publication),
        |path, record| {
            write_record_with_publisher(path, record, |temporary, destination| {
                replace_automation_file_atomically(
                    temporary,
                    destination,
                    "automation terminal journal",
                )?;
                Err(std::io::Error::other(
                    "injected error after hardened classifier republication",
                ))
            })
        },
    )
    .expect_err("failed exact republication must prevent Terminal classification");
    assert!(
        classifier_error
            .to_string()
            .contains("hardened classifier republication")
    );
    let promotion_error = promote_prepared_terminal_with_writers(
        &journal_path,
        &admission,
        terminal.clone(),
        &publication,
        |path, record| {
            write_record_with_publisher(path, record, |temporary, destination| {
                replace_automation_file_atomically(
                    temporary,
                    destination,
                    "automation terminal journal",
                )?;
                Err(std::io::Error::other(
                    "injected error after hardened promotion republication",
                ))
            })
        },
        write_record,
    )
    .expect_err("failed exact republication must prevent Terminal promotion replay");
    assert!(
        promotion_error
            .to_string()
            .contains("hardened promotion republication")
    );
    assert_eq!(exact_spool_file_count(dashboard_root), 1);
    assert_eq!(
        recovery_index::indexed_journals_blocking(dashboard_root, &admission.scope)
            .expect("pending authority after failed republication")
            .len(),
        1
    );

    assert_eq!(
        classify_durable_settlement_blocking(
            &journal_path,
            &admission,
            &terminal,
            Some(&publication),
        )
        .expect("hardened exact republish and reread visible terminal"),
        DurableSettlementClassification::Terminal
    );
    assert_eq!(
        promote_prepared_terminal_blocking(
            &journal_path,
            &admission,
            terminal.clone(),
            &publication,
        )
        .expect("exact terminal retry"),
        terminal
    );
    tracedecay_agent_hosts::automation::run_ledger::discard_staged_run_record_exact_blocking(
        dashboard_root,
        run_id,
        &publication,
    )
    .expect("cleanup only after exact terminal retry");
    recovery_index::remove_pending_blocking(dashboard_root, &journal_path)
        .expect("retire pending authority after exact retry");
    assert_eq!(exact_spool_file_count(dashboard_root), 0);
    assert!(
        recovery_index::indexed_journals_blocking(dashboard_root, &admission.scope)
            .expect("retired pending authority")
            .is_empty()
    );
    drop(claim);
    drop(guard);
}

#[test]
fn uncertain_bind_replay_distinguishes_reserved_from_durable_prepared() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("uncertain-bind.json");
    let admission = admission("run.uncertain-bind", "request.uncertain-bind");
    let claim = reserve_or_replay_blocking(&path, admission.clone()).expect("reserve");
    let terminal = success_terminal(&admission, "run.uncertain-bind");
    let publication = exact_publication('c', 16_384);

    assert_eq!(
        replay_exact_binding_after_error_blocking(&path, &admission, &terminal, &publication,)
            .expect("reserved classification"),
        None
    );
    persist_prepared_terminal_blocking(&path, &admission, &terminal, publication.clone())
        .expect("prepared");
    assert_eq!(
        replay_exact_binding_after_error_blocking(&path, &admission, &terminal, &publication,)
            .expect("prepared classification"),
        Some(terminal)
    );
    drop(claim);
}

#[test]
fn every_durable_state_is_exactly_republished_before_classification() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("republish-every-state.json");
    let admission = admission("run.republish-every-state", "request.republish-every-state");
    let claim = reserve_or_replay_blocking(&path, admission.clone()).expect("reserve");
    let terminal = success_terminal(&admission, "run.republish-every-state");
    let publication = exact_publication('b', 32_768);
    let republishes = std::cell::Cell::new(0_usize);

    let reserved = classify_durable_settlement_with_stabilizer(
        &path,
        &admission,
        &terminal,
        Some(&publication),
        |path, record| {
            republishes.set(republishes.get() + 1);
            write_record(path, record)
        },
    )
    .expect("classify republished Reserved");
    assert_eq!(reserved, DurableSettlementClassification::Reserved);
    assert_eq!(republishes.get(), 1);

    persist_prepared_terminal_blocking(&path, &admission, &terminal, publication.clone())
        .expect("prepare terminal");
    let prepared = classify_durable_settlement_with_stabilizer(
        &path,
        &admission,
        &terminal,
        Some(&publication),
        |path, record| {
            republishes.set(republishes.get() + 1);
            write_record(path, record)
        },
    )
    .expect("classify republished Prepared");
    assert_eq!(prepared, DurableSettlementClassification::Prepared);
    assert_eq!(republishes.get(), 2);

    promote_prepared_terminal_blocking(&path, &admission, terminal.clone(), &publication)
        .expect("promote terminal");
    let terminal_state = classify_durable_settlement_with_stabilizer(
        &path,
        &admission,
        &terminal,
        Some(&publication),
        |path, record| {
            republishes.set(republishes.get() + 1);
            write_record(path, record)
        },
    )
    .expect("classify republished Terminal");
    assert_eq!(terminal_state, DurableSettlementClassification::Terminal);
    assert_eq!(republishes.get(), 3);
    drop(claim);
}

#[test]
fn failed_exact_republication_never_proves_visible_prepared_state() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("visible-but-undurable-prepared.json");
    let admission = admission(
        "run.visible-but-undurable-prepared",
        "request.visible-but-undurable-prepared",
    );
    reserve_or_replay_blocking(&path, admission.clone()).expect("reserve");
    let terminal = success_terminal(&admission, "run.visible-but-undurable-prepared");
    persist_prepared_terminal_blocking(&path, &admission, &terminal, exact_publication('d', 4_096))
        .expect("prepared");
    let visible = read_record(&path).expect("read").expect("visible record");

    let error = stabilize_bound_record_after_visibility_with(&path, &visible, |_, _| {
        Err(contract_error(
            "injected exact journal republication failure",
        ))
    })
    .expect_err("visible journal without exact republication must not prove durability");
    assert!(
        error
            .to_string()
            .contains("exact journal republication failure")
    );
}

#[test]
fn bound_state_stabilization_rereads_after_parent_sync() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("changed-during-stabilization.json");
    let admission = admission(
        "run.changed-during-stabilization",
        "request.changed-during-stabilization",
    );
    reserve_or_replay_blocking(&path, admission.clone()).expect("reserve");
    let terminal = success_terminal(&admission, "run.changed-during-stabilization");
    persist_prepared_terminal_blocking(&path, &admission, &terminal, exact_publication('e', 8_192))
        .expect("prepared");
    let visible = read_record(&path).expect("read").expect("visible record");
    let replacement = DurableAutomationRecord {
        admission,
        state: DurableAutomationState::Reserved,
        legacy_terminal: None,
    };

    let error = stabilize_bound_record_after_visibility_with(&path, &visible, |path, _| {
        write_record(path, &replacement)
    })
    .expect_err("state changed during republication must fail readback validation");
    assert!(error.to_string().contains("changed while stabilizing"));
}

#[test]
fn oversized_journal_prewrite_preserves_the_valid_reservation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("bounded-terminal.json");
    let admission = admission("run.bounded-journal", "request.bounded-journal");
    reserve_or_replay_blocking(&path, admission.clone()).expect("reserve");
    let before = std::fs::read(&path).expect("reserved bytes");
    let mut oversized_admission = admission.clone();
    oversized_admission.process_run_id = "x".repeat(MAX_AUTOMATION_JOURNAL_BYTES as usize);
    let oversized = DurableAutomationRecord {
        admission: oversized_admission,
        state: DurableAutomationState::Terminal {
            terminal: terminal_binding(&success_terminal(&admission, "run.bounded-journal"))
                .expect("binding"),
            publication: None,
        },
        legacy_terminal: None,
    };

    assert!(write_record(&path, &oversized).is_err());
    assert_eq!(std::fs::read(&path).expect("preserved bytes"), before);
    assert!(matches!(
        read_record(&path)
            .expect("read preserved reservation")
            .expect("reservation")
            .state,
        DurableAutomationState::Reserved
    ));
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
fn durable_journal_reports_changed_request_identity_as_a_conflict() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("terminal.json");
    reserve_or_replay_blocking(
        &path,
        admission("run.memory-journal", "request.memory-journal"),
    )
    .expect("reserve");
    let changed = admission("run.memory-journal", "request.memory-journal.changed");
    assert_admission_conflict(reserve_or_replay_blocking(&path, changed));
}

#[test]
fn durable_journal_reports_changed_task_identity_as_a_conflict() {
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
    let changed_problem = reset_problem(&changed.request_id, &changed.scope, &changed.request);
    let AutomationRecoveryBinding::Memory {
        recovery_problem, ..
    } = &mut changed.recovery
    else {
        panic!("memory admission must carry memory recovery")
    };
    *recovery_problem = changed_problem;
    let changed = seal_effect_authority(changed);
    assert_admission_conflict(reserve_or_replay_blocking(&path, changed));
}

#[test]
fn durable_journal_reports_changed_scope_identity_as_a_conflict() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("terminal.json");
    let original = admission("run.memory-journal", "request.memory-journal");
    reserve_or_replay_blocking(&path, original).expect("reserve");
    let mut changed = admission("run.memory-journal", "request.memory-journal");
    let changed_scope = ResolvedScope::new(
        ProjectId::new("project.memory-journal.other").expect("project"),
        RepositoryId::new("repository.memory-journal").expect("repository"),
        WorktreeId::new("worktree.memory-journal").expect("worktree"),
        None,
    )
    .expect("scope");
    changed.scope = changed_scope.clone();
    changed.effect_receipt_template.scope = changed_scope.clone();
    let changed_problem = reset_problem(&changed.request_id, &changed_scope, &changed.request);
    let AutomationRecoveryBinding::Memory {
        owner,
        recovery_problem,
        ..
    } = &mut changed.recovery
    else {
        panic!("memory admission must carry memory recovery")
    };
    *owner = FactOwnerV1::Project {
        project_id: changed_scope.project_id.clone(),
    };
    *recovery_problem = changed_problem;
    let changed = seal_effect_authority(changed);
    assert_admission_conflict(reserve_or_replay_blocking(&path, changed));
}

#[test]
fn durable_journal_reports_changed_effect_authority_as_a_conflict() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("terminal.json");
    let original = admission("run.memory-journal", "request.memory-journal");
    reserve_or_replay_blocking(&path, original.clone()).expect("reserve");
    let mut changed = original;
    changed.effect_authority_digest = digest('b');
    assert_admission_conflict(reserve_or_replay_blocking(&path, changed));
}

#[test]
fn recovery_authority_digest_rejects_every_mutable_recovery_and_digest_domain() {
    let original = admission("run.authority-binding", "request.authority-binding");
    assert_effect_authority(&original, true, "canonical memory admission");
    let mut restarted = original.clone();
    restarted.process_run_id = "process.authority-binding.restarted".to_owned();
    assert_effect_authority(
        &restarted,
        true,
        "process identity is intentionally outside stable effect authority",
    );

    let mut mutations = Vec::new();
    let mut changed = original.clone();
    changed.input_digest = digest('b');
    mutations.push(("automation-run input digest", changed));

    let mut changed = original.clone();
    changed.effect_receipt_template.input_digest = digest('c');
    mutations.push(("retained-effect receipt input digest", changed));

    let mut changed = original.clone();
    let AutomationRecoveryBinding::Memory { owner, .. } = &mut changed.recovery else {
        panic!("memory admission must carry memory recovery")
    };
    *owner = FactOwnerV1::Project {
        project_id: ProjectId::new("project.authority-binding.other").expect("project"),
    };
    mutations.push(("memory recovery owner", changed));

    let changed_problem = reset_problem(
        &RequestId::new("request.authority-binding.changed").expect("request id"),
        &original.scope,
        &original.request,
    );
    let mut changed = original.clone();
    let AutomationRecoveryBinding::Memory {
        recovery_problem, ..
    } = &mut changed.recovery
    else {
        panic!("memory admission must carry memory recovery")
    };
    *recovery_problem = changed_problem;
    mutations.push(("memory recovery problem", changed));

    let mut changed = original.clone();
    let AutomationRecoveryBinding::Memory { retirement, .. } = &mut changed.recovery else {
        panic!("memory admission must carry memory recovery")
    };
    *retirement = Some(super::super::retirement::RetirementBinding {
        source_digest: format!("sha256:{}", "d".repeat(64)),
        archive_name: format!("fact_proposals.{}.json", "d".repeat(64)),
    });
    mutations.push(("memory retirement", changed));

    let mut changed = original.clone();
    let AutomationRecoveryBinding::Memory {
        reset_source_digest,
        ..
    } = &mut changed.recovery
    else {
        panic!("memory admission must carry memory recovery")
    };
    *reset_source_digest = Some(format!("sha256:{}", "e".repeat(64)));
    mutations.push(("memory reset source", changed));

    let external = external_admission_for_job(
        "run.external-authority-binding",
        "request.external-authority-binding",
        "authority-binding",
    );
    assert_effect_authority(&external, true, "canonical external admission");
    let external_problem = reset_problem(
        &RequestId::new("request.external-authority-binding.changed").expect("request id"),
        &external.scope,
        &external.request,
    );
    let mut changed = external;
    let AutomationRecoveryBinding::External { recovery_problem } = &mut changed.recovery else {
        panic!("external admission must carry external recovery")
    };
    *recovery_problem = external_problem;
    mutations.push(("external recovery problem", changed));

    for (label, changed) in mutations {
        assert_effect_authority(&changed, false, label);
    }
}

#[test]
fn divergent_recovery_owner_is_rejected_without_downgrading_the_reservation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("terminal.json");
    let original = admission("run.memory-journal", "request.memory-journal");
    reserve_or_replay_blocking(&path, original).expect("reserve");
    let before = std::fs::read(&path).expect("reserved bytes");
    let mut changed = admission("run.memory-journal", "request.memory-journal");
    let AutomationRecoveryBinding::Memory { owner, .. } = &mut changed.recovery else {
        panic!("memory admission must carry memory recovery")
    };
    // The admission contract pins the memory recovery owner to the scope's
    // project identity, so a divergent owner is an invalid admission shape
    // and must be rejected before any durable comparison or write.
    *owner = FactOwnerV1::Project {
        project_id: ProjectId::new("project.memory-journal.other").expect("project"),
    };
    assert!(reserve_or_replay_blocking(&path, changed).is_err());
    assert_eq!(std::fs::read(&path).expect("preserved reservation"), before);
}

#[test]
fn identical_same_process_reservation_remains_an_in_flight_contract_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("terminal.json");
    let original = admission("run.memory-journal", "request.memory-journal");
    let live = reserve_or_replay_blocking(&path, original.clone()).expect("reserve");

    let Err(error) = reserve_or_replay_blocking(&path, original) else {
        panic!("an identical live reservation must remain a contract error")
    };
    assert!(error.to_string().contains("already in flight"));
    drop(live);
}

#[test]
fn abandoned_same_process_reservation_enters_recovery_without_reexecution() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("terminal.json");
    let original = admission("run.abandoned-journal", "request.abandoned-journal");
    let claim = reserve_or_replay_blocking(&path, original.clone()).expect("reserve");
    drop(claim);

    assert!(matches!(
        reserve_or_replay_blocking(&path, original).expect("recover dropped authority"),
        ReservationResult::Recover { .. }
    ));
}

#[tokio::test]
async fn direct_recover_retires_spool_staged_before_prepared_binding() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("direct-recover-orphan.json");
    let original = admission("run.direct-recover-orphan", "request.direct-recover-orphan");
    let claim = reserve_or_replay_blocking(&path, original.clone()).expect("reserve");
    let ledger: AutomationRunLedgerRecord = serde_json::from_value(json!({
        "schema_version": 2,
        "run_id": original.request.run_id.as_str(),
        "trigger": "dashboard",
        "task": "memory_curator",
        "backend": "codex_app_server",
        "status": "skipped",
        "accepted_count": 0,
        "rejected_count": 0,
        "error": "no_memory_curator_evidence",
        "fallback_status": "no_memory_curator_evidence",
        "started_at": "1",
        "completed_at": "2"
    }))
    .expect("ledger record");
    tracedecay_agent_hosts::automation::run_ledger::bind_staged_run_record_exact(
        temp.path(),
        &ledger,
        |_| Ok(()),
    )
    .expect("stage without journal binding");
    let spool_dir = temp.path().join("automation_run_spool");
    assert_eq!(
        std::fs::read_dir(&spool_dir)
            .expect("spool directory")
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .count(),
        1
    );
    drop(claim);
    assert!(matches!(
        reserve_or_replay_blocking(&path, original.clone()).expect("direct recover"),
        ReservationResult::Recover { .. }
    ));

    super::super::discard_direct_recovery_unbound_spools(temp.path(), &path, &original)
        .await
        .expect("discard abandoned spool");

    assert_eq!(
        std::fs::read_dir(&spool_dir)
            .expect("spool directory")
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .count(),
        0
    );
    let terminal = success_terminal(&original, "run.direct-recover-orphan");
    assert_eq!(
        persist_recovered_terminal_blocking(&path, &original, terminal.clone(), None)
            .expect("settle recovered terminal"),
        Some(terminal)
    );
    let record = read_indexed_record_blocking(&path)
        .expect("journal")
        .expect("terminal record");
    assert!(record.is_terminal());
    assert!(record.publication().is_none());
    assert_eq!(
        std::fs::read_dir(&spool_dir)
            .expect("spool directory")
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .count(),
        0
    );
}

#[test]
fn unbound_cleanup_requires_abandoned_reserved_state_at_revalidation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("terminal.json");
    let original = admission("run.cleanup-claim", "request.cleanup-claim");
    let live = reserve_or_replay_blocking(&path, original.clone()).expect("reserve");
    assert!(
        !unbound_reserved_cleanup_is_safe_blocking(&path, &original).expect("live revalidation")
    );

    drop(live);
    assert!(
        unbound_reserved_cleanup_is_safe_blocking(&path, &original)
            .expect("abandoned revalidation")
    );

    let terminal = success_terminal(&original, "run.cleanup-claim");
    persist_prepared_terminal_blocking(&path, &original, &terminal, exact_publication('d', 4_096))
        .expect("prepared");
    assert!(
        !unbound_reserved_cleanup_is_safe_blocking(&path, &original)
            .expect("prepared revalidation")
    );
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
    assert!(!reopened_record.is_terminal());
    assert!(matches!(
        reserve_or_replay_blocking(&path, reopened)
            .expect("deferred retirement remains recoverable"),
        ReservationResult::Recover { .. }
    ));
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
    let plan = match super::super::retirement::classify_for_task(
        AutomationTaskV1::SessionReflector,
        &dashboard_root,
    )
    .await
    .expect("classify exact retirement source")
    {
        super::super::retirement::RetirementClassification::Terminal(plan) => plan,
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
        tracedecay_agent_hosts::automation::run_ledger::bind_staged_run_record_exact(
            &dashboard_root,
            &anchor.ledger_record,
            |publication| Ok(publication.clone()),
        )
        .expect("stage exact anchor ledger row");
    assert_eq!(
        tracedecay_agent_hosts::automation::run_ledger::publish_staged_run_record_exact(
            &dashboard_root,
            &anchor.ledger_record.run_id,
            &anchor_publication,
        )
        .await
        .expect("publish exact anchor ledger row"),
        tracedecay_agent_hosts::automation::run_ledger::ExactRunPublishOutcome::Published
    );
    tracedecay_agent_hosts::automation::run_ledger::discard_staged_run_record_exact(
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
        tracedecay_agent_hosts::automation::run_ledger::run_ledger_path(&dashboard_root);
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
    let failed = recovery_index::reconcile_reserved_automation_effects_for_project(
        &cg,
        &dashboard_root,
        &tracedecay_application::CancellationSignal::active(
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
    let pending_retirement = super::super::retirement::finalize_after_terminal(
        &dashboard_root,
        &plan.binding,
        Some(&plan),
    )
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
    super::super::retirement::complete_after_pending_removal(&pending_retirement)
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
    let orphaned_retirement = super::super::retirement::finalize_after_terminal(
        &dashboard_root,
        &plan.binding,
        Some(&plan),
    )
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
    let rejected = recovery_index::reconcile_reserved_automation_effects_for_project(
        &reopened,
        &dashboard_root,
        &tracedecay_application::CancellationSignal::active(
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
    let recovered = recovery_index::reconcile_reserved_automation_effects_for_project(
        &reopened,
        &dashboard_root,
        &tracedecay_application::CancellationSignal::active(
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
    let entry_plus_marker_retirement = super::super::retirement::finalize_after_terminal(
        &dashboard_root,
        &plan.binding,
        Some(&plan),
    )
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

    let entry_plus_marker = recovery_index::reconcile_reserved_automation_effects_for_project(
        &reopened,
        &dashboard_root,
        &tracedecay_application::CancellationSignal::active(
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
        tracedecay_agent_hosts::automation::run_ledger::find_run_record_exact_bounded_blocking(
            &dashboard_root,
            &anchor.ledger_record.run_id,
        )
        .expect("exact anchor lookup"),
        Some(anchor.ledger_record)
    );

    recovery_index::add_pending_blocking(&dashboard_root, &journal_path, &admission)
        .expect("re-index exact Terminal for idempotent retry");
    let replayed = recovery_index::reconcile_reserved_automation_effects_for_project(
        &reopened,
        &dashboard_root,
        &tracedecay_application::CancellationSignal::active(
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
        recovery_index::reconcile_reserved_automation_effects_for_project(
            &reopened,
            &dashboard_root,
            &tracedecay_application::CancellationSignal::active(
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
    assert!(!reopened_record.is_terminal());
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
    // The durable writer fail-closes on a swapped terminal, so the corrupt
    // on-disk state must be staged as raw bytes to model foreign corruption.
    let corrupt = DurableAutomationRecord {
        admission: admission.clone(),
        state: DurableAutomationState::Terminal {
            terminal: write_terminal_sidecar(
                &path,
                &success_terminal(&admission, "run.memory-journal.other"),
            )
            .expect("swapped sidecar"),
            publication: None,
        },
        legacy_terminal: None,
    };
    write_private_test_file(
        &path,
        &serde_json::to_vec_pretty(&corrupt).expect("corrupt fixture bytes"),
    );
    assert!(reserve_or_replay_blocking(&path, admission).is_err());
}

// The scheduler module itself is unix-only; its request-identity contract
// can only be exercised where it compiles.
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

#[test]
fn pending_index_project_binding_conflict_remains_a_contract_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dashboard_root = temp.path();
    let path = dashboard_root
        .join("automation_effects")
        .join(format!("{}.json", "a".repeat(64)));
    let original = admission("indexed_binding", "request.indexed-binding");
    recovery_index::add_pending_blocking(dashboard_root, &path, &original)
        .expect("original pending binding");

    let mut changed = original;
    changed.scope = ResolvedScope::new(
        ProjectId::new("project.memory-journal.other").expect("project"),
        RepositoryId::new("repository.memory-journal").expect("repository"),
        WorktreeId::new("worktree.memory-journal").expect("worktree"),
        None,
    )
    .expect("scope");
    let Err(error) = recovery_index::add_pending_blocking(dashboard_root, &path, &changed) else {
        panic!("pending index must preserve its original project binding")
    };
    assert!(error.to_string().contains("project binding"));
}

#[tokio::test]
async fn reserved_admission_conflict_preserves_recovery_index() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dashboard_root = temp.path();
    let path = dashboard_root
        .join("automation_effects")
        .join(format!("{}.json", "b".repeat(64)));
    let original = admission("indexed_conflict", "request.indexed-conflict");
    recovery_index::add_pending_blocking(dashboard_root, &path, &original)
        .expect("pending reservation");
    reserve_or_replay_blocking(&path, original.clone()).expect("reserve");
    let mut changed = original;
    changed.configuration_digest = digest('c');
    changed.effect_receipt_template.configuration_digest = digest('c');
    let changed = seal_effect_authority(changed);
    let ReservationResult::Conflict { terminal } =
        reserve_or_replay_blocking(&path, changed).expect("typed conflict")
    else {
        panic!("stable mismatch must be a conflict")
    };
    assert!(!terminal);
    let admission = super::super::reservation_conflict_admission(dashboard_root, &path, terminal)
        .await
        .expect("map conflict");
    assert!(matches!(
        admission,
        super::super::AutomationEffectAdmission::Conflict
    ));
    assert_eq!(
        recovery_index::indexed_journals_blocking(dashboard_root, &scope())
            .expect("pending index")
            .len(),
        1
    );
}

#[tokio::test]
async fn terminal_admission_conflict_preserves_existing_cleanup_authority() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dashboard_root = temp.path();
    let path = dashboard_root
        .join("automation_effects")
        .join(format!("{}.json", "c".repeat(64)));
    let original = admission("terminal_conflict", "request.terminal-conflict");
    reserve_or_replay_blocking(&path, original.clone()).expect("reserve");
    persist_terminal_blocking(
        &path,
        &original,
        success_terminal(&original, "terminal_conflict"),
    )
    .expect("terminal");
    let mut changed = original;
    changed.configuration_digest = digest('d');
    changed.effect_receipt_template.configuration_digest = digest('d');
    let changed = seal_effect_authority(changed);
    recovery_index::add_pending_blocking(dashboard_root, &path, &changed)
        .expect("prepare recreates pending entry before reservation lookup");
    let ReservationResult::Conflict { terminal } =
        reserve_or_replay_blocking(&path, changed).expect("typed conflict")
    else {
        panic!("terminal mismatch must be a conflict")
    };
    assert!(terminal);
    let admission = super::super::reservation_conflict_admission(dashboard_root, &path, terminal)
        .await
        .expect("map conflict");
    assert!(matches!(
        admission,
        super::super::AutomationEffectAdmission::Conflict
    ));
    assert_eq!(
        recovery_index::indexed_journals_blocking(dashboard_root, &scope())
            .expect("pending index")
            .len(),
        1
    );
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

    let report = recovery_index::reconcile_reserved_automation_effects_for_project(
        &cg,
        dashboard_root,
        &tracedecay_application::CancellationSignal::active("cancellation.post-write-reservation")
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

    let report = recovery_index::reconcile_reserved_automation_effects_for_project(
        &cg,
        dashboard_root,
        &tracedecay_application::CancellationSignal::active("cancellation.prewrite-reservation")
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

    let report = recovery_index::reconcile_reserved_automation_effects_for_project(
        &cg,
        dashboard_root,
        &tracedecay_application::CancellationSignal::active(
            "cancellation.clean-eof-corrupt-intent",
        )
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
            &crate::daemon::project_open_owners::resolved_scope_for_project(
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
        tracedecay_agent_hosts::automation::run_ledger::bind_staged_run_record_exact(
            dashboard_root,
            &expected_record,
            |publication| Ok(publication.clone()),
        )
        .expect("stage exact recovery spool");
    let spool_files = exact_spool_files(dashboard_root);
    assert_eq!(spool_files.len(), 1);
    let spool = std::fs::read(&spool_files[0]).expect("exact spool payload");
    let ledger_path =
        tracedecay_agent_hosts::automation::run_ledger::run_ledger_path(dashboard_root);
    std::fs::write(&ledger_path, &spool[..spool.len() / 2]).expect("owned partial ledger tail");
    let intent_path = dashboard_root.join("automation_runs.jsonl.append-intent");
    std::fs::write(&intent_path, b"corrupt-partial-intent").expect("corrupt append intent");

    let report = recovery_index::reconcile_reserved_automation_effects_for_project(
        &cg,
        dashboard_root,
        &tracedecay_application::CancellationSignal::active("cancellation.partial-corrupt-intent")
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
        tracedecay_agent_hosts::automation::run_ledger::publish_staged_run_record_exact(
            dashboard_root,
            run_id,
            &publication,
        )
        .await
        .expect("publish repaired exact row"),
        tracedecay_agent_hosts::automation::run_ledger::ExactRunPublishOutcome::Published
    );
    assert_eq!(
        tracedecay_agent_hosts::automation::run_ledger::find_run_record_exact_bounded_blocking(
            dashboard_root,
            run_id,
        )
        .expect("exact repaired row"),
        Some(expected_record)
    );
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
    assert!(!record.is_terminal());
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
        !read_indexed_record_blocking(&path)
            .expect("read reservation")
            .expect("record")
            .is_terminal()
    );
}

#[test]
fn retained_settlement_waiter_is_send_and_static() {
    fn assert_send_static<T: Send + 'static>() {}

    assert_send_static::<super::super::RetainedSettlementWaiter<crate::errors::Result<()>>>();
    assert_send_static::<
        super::super::RetainedSettlementWaiter<
            crate::errors::Result<(
                super::super::AutomationSettledTerminal,
                AutomationRunLedgerRecord,
            )>,
        >,
    >();
    assert_send_static::<
        super::super::RetainedSettlementWaiter<
            crate::errors::Result<(
                super::super::AutomationSettledProblem,
                Option<AutomationRunLedgerRecord>,
            )>,
        >,
    >();
    assert_send_static::<super::super::RetainedSettlementPairWaiter>();
    assert_send_static::<super::super::DeferredSettlementPairSubmission<()>>();
    assert_send_static::<
        super::super::RetainedSettlementWaiter<
            crate::errors::Result<super::super::RetainedAutomationSettlementOutcome>,
        >,
    >();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_retained_waiter_does_not_abort_blocking_owner() {
    let (started_tx, started_rx) = std::sync::mpsc::sync_channel(0);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
    let (finished_tx, finished_rx) = std::sync::mpsc::sync_channel(0);
    let waiter = super::super::RetainedSettlementWaiter {
        task: tokio::task::spawn_blocking(move || {
            started_tx.send(()).expect("signal blocking owner");
            release_rx.recv().expect("release blocking owner");
            finished_tx.send(()).expect("signal owner completion");
            crate::errors::Result::Ok(())
        }),
    };

    started_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("blocking owner started");
    drop(waiter);
    release_tx.send(()).expect("release detached owner");
    finished_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("detached owner completed after waiter drop");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reused_scheduler_skip_abandons_current_effect_before_observing_exact_prior() {
    use fs2::FileExt;
    use std::sync::Arc;
    use std::time::Duration;
    use tracedecay_agent_hosts::automation::AutomationRunControl;
    use tracedecay_agent_hosts::automation::backend::{AgentTaskKind, task_key};
    use tracedecay_agent_hosts::automation::config::{
        AutomationBackend, AutomationConfig, AutomationHostMode,
    };
    use tracedecay_agent_hosts::automation::run_ledger::AutomationTrigger;
    use tracedecay_agent_hosts::automation::runner::{
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
        &cg,
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
    );
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
        tracedecay_agent_hosts::automation::run_ledger::run_ledger_path(dashboard_root);
    let prior_ledger_bytes = std::fs::read(&ledger_path).expect("prior exact ledger bytes");
    assert_eq!(
        tracedecay_agent_hosts::automation::run_ledger::find_run_record_exact_bounded_blocking(
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
        );
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
        );
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
    );
    recovery_index::add_pending_blocking(dashboard_root, &current_journal, &current_admission)
        .expect("current pending authority");

    let journal_lock_path = crate::storage::append_lock_path(&current_journal);
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
        tracedecay_agent_hosts::automation::run_ledger::find_run_record_exact_bounded_blocking(
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
        tracedecay_agent_hosts::automation::run_ledger::find_run_record_exact_bounded_blocking(
            dashboard_root,
            current_run_id,
        )
        .expect("current exact absence after abandonment")
        .is_none()
    );
    assert_eq!(exact_spool_files(dashboard_root), prior_spool_files);
    assert!(
        tracedecay_agent_hosts::automation::scheduler::AutomationTaskLock::try_acquire(
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retained_user_job_rebinds_and_recovery_retires_only_terminal_corrupt_spool() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

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
    .expect("initialize retained cleanup recovery project");
    let dashboard_root = &cg.store_layout().dashboard_root;
    let run_id = "run.retained-user-job-drop";
    let job_id = "retained-drop";
    let retained = retained_disabled_user_job_run(dashboard_root, run_id, job_id).await;
    let owner = cg.project_memory_owner().expect("project memory owner");
    let FactOwnerV1::Project { project_id } = owner else {
        panic!("retained cleanup recovery requires a project owner")
    };
    let recovery_scope = crate::daemon::project_open_owners::resolved_scope_for_project(
        cg.project_root(),
        &project_id,
    )
    .expect("recovery scope");
    let mut cleanup_admission =
        external_admission_for_job(run_id, "request.retained-user-job-drop", job_id);
    cleanup_admission.scope = recovery_scope.clone();
    cleanup_admission.effect_receipt_template.scope = recovery_scope.clone();
    cleanup_admission.recovery = AutomationRecoveryBinding::External {
        recovery_problem: reset_problem(
            &cleanup_admission.request_id,
            &recovery_scope,
            &cleanup_admission.request,
        ),
    };
    cleanup_admission = seal_effect_authority(cleanup_admission);
    let (authority, journal_path, expected_admission) =
        retained_external_authority(dashboard_root, cleanup_admission);
    recovery_index::add_pending_blocking(dashboard_root, &journal_path, &expected_admission)
        .expect("retain settlement cleanup authority");

    let (phase_tx, phase_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let phase_release = Arc::clone(&release_rx);
    let phase_hook = super::super::SettlementPhaseHook::new(move |phase| {
        phase_tx.send(phase).expect("publish settlement phase");
        phase_release
            .lock()
            .expect("phase release lock")
            .recv()
            .expect("release settlement phase");
    });
    let publications = Arc::new(Mutex::new(Vec::new()));
    let attempted_publications = Arc::clone(&publications);
    let fail_once = Arc::new(AtomicBool::new(true));
    let prepared_fail_once = Arc::clone(&fail_once);
    let prepared_write_hook = super::super::PreparedWriteHook::new(move |publication| {
        attempted_publications
            .lock()
            .expect("publication attempts")
            .push(publication.clone());
        if prepared_fail_once.swap(false, Ordering::SeqCst) {
            return Err(super::super::contract_error(
                "injected prepared journal write failure",
            ));
        }
        Ok(())
    });
    let (observed_tx, observed_rx) = std::sync::mpsc::channel();
    let (projected_tx, projected_rx) = std::sync::mpsc::channel();
    let waiter = authority.start_retained_automation_settlement_with_phase_hooks(
        retained,
        Some(Box::new(move |record| {
            observed_tx
                .send(record.clone())
                .expect("exact row observation");
        })),
        move |run| {
            projected_tx
                .send(run.ledger_record.clone())
                .expect("project exact retained row");
            (run.ledger_record, run.committed_receipt)
        },
        phase_hook,
        Some(prepared_write_hook),
    );
    drop(waiter);
    let expected_record = projected_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("projector executed inside detached owner");

    assert_eq!(
        phase_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("unbound retry phase"),
        super::super::RetainedSettlementPhase::PreparedWriteFailed
    );
    let reserved = read_indexed_record_blocking(&journal_path)
        .expect("reserved journal read")
        .expect("reserved journal");
    assert!(!reserved.is_terminal());
    assert!(reserved.prepared().is_none());
    assert_eq!(reserved.admission(), &expected_admission);
    assert!(
        tracedecay_agent_hosts::automation::run_ledger::find_run_record_exact_bounded_blocking(
            dashboard_root,
            run_id,
        )
        .expect("exact lookup after failed Prepared write")
        .is_none()
    );
    assert_eq!(exact_spool_file_count(dashboard_root), 1);
    assert!(task_lock_is_denied(dashboard_root, job_id).await);

    release_tx.send(()).expect("release failed Prepared phase");
    assert_eq!(
        phase_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("prepared phase"),
        super::super::RetainedSettlementPhase::Prepared
    );
    assert!(
        read_indexed_record_blocking(&journal_path)
            .expect("prepared journal read")
            .expect("prepared journal")
            .prepared()
            .is_some()
    );
    assert!(
        tracedecay_agent_hosts::automation::run_ledger::find_run_record_exact_bounded_blocking(
            dashboard_root,
            run_id,
        )
        .expect("exact lookup before publication")
        .is_none()
    );
    let publication = {
        let attempted_publications = publications.lock().expect("publication attempts");
        assert_eq!(attempted_publications.len(), 2);
        assert_eq!(attempted_publications[0], attempted_publications[1]);
        attempted_publications[1].clone()
    };
    assert_eq!(exact_spool_file_count(dashboard_root), 1);
    assert!(task_lock_is_denied(dashboard_root, job_id).await);

    release_tx.send(()).expect("release prepared phase");
    assert_eq!(
        phase_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("published phase"),
        super::super::RetainedSettlementPhase::Published
    );
    assert_eq!(
        tracedecay_agent_hosts::automation::run_ledger::find_run_record_exact_bounded_blocking(
            dashboard_root,
            run_id,
        )
        .expect("exact lookup after publication"),
        Some(expected_record.clone())
    );
    let prepared = read_indexed_record_blocking(&journal_path)
        .expect("published journal read")
        .expect("published journal");
    assert!(prepared.prepared().is_some());
    assert!(!prepared.is_terminal());
    assert_eq!(prepared.admission(), &expected_admission);
    assert!(task_lock_is_denied(dashboard_root, job_id).await);
    let spool_files = exact_spool_files(dashboard_root);
    assert_eq!(spool_files.len(), 1);
    let spool_path = spool_files[0].clone();
    std::fs::write(&spool_path, b"corrupt-after-exact-publication")
        .expect("corrupt exact stable spool");
    let prepared_terminal = read_indexed_terminal_blocking(&journal_path)
        .expect("prepared terminal sidecar")
        .expect("prepared terminal");
    let cleanup_path = journal_path.clone();
    let cleanup_admission = expected_admission.clone();
    let cleanup_terminal = prepared_terminal.clone();
    let cleanup_publication = publication.clone();
    let cleanup_error = tracedecay_agent_hosts::automation::run_ledger::discard_stale_staged_run_record_exact_after_terminal(
        dashboard_root,
        run_id,
        &publication,
        move || {
            Ok(classify_durable_settlement_blocking(
                &cleanup_path,
                &cleanup_admission,
                &cleanup_terminal,
                Some(&cleanup_publication),
            )?
            .is_terminal())
        },
    )
    .await
    .expect_err("Prepared journal must not authorize stale spool retirement");
    assert!(
        cleanup_error
            .to_string()
            .contains("lacks matching terminal authority")
    );
    assert_eq!(exact_spool_file_count(dashboard_root), 1);
    assert_eq!(
        recovery_index::indexed_journals_blocking(dashboard_root, &expected_admission.scope)
            .expect("pre-Terminal pending authority")
            .len(),
        1
    );

    release_tx.send(()).expect("release published phase");
    assert_eq!(
        observed_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("detached exact observation"),
        expected_record.clone()
    );
    assert!(
        observed_rx.try_recv().is_err(),
        "observer must run exactly once"
    );
    let terminal_journal = read_indexed_record_blocking(&journal_path)
        .expect("terminal journal read")
        .expect("terminal journal");
    assert!(terminal_journal.is_terminal());
    assert_eq!(terminal_journal.admission(), &expected_admission);
    assert_eq!(
        tracedecay_agent_hosts::automation::run_ledger::find_run_record_exact_bounded_blocking(
            dashboard_root,
            run_id,
        )
        .expect("exact lookup after terminal"),
        Some(expected_record.clone())
    );
    let exact_rows = std::fs::read_to_string(
        tracedecay_agent_hosts::automation::run_ledger::run_ledger_path(dashboard_root),
    )
    .expect("physical exact run ledger")
    .lines()
    .filter(|line| !line.is_empty())
    .map(|line| serde_json::from_str::<AutomationRunLedgerRecord>(line).expect("exact ledger row"))
    .filter(|record| record.run_id == run_id)
    .collect::<Vec<_>>();
    assert_eq!(
        exact_rows,
        vec![expected_record.clone()],
        "physical ledger must contain the exact run row once"
    );
    assert_eq!(exact_spool_file_count(dashboard_root), 1);
    let pending =
        recovery_index::indexed_journals_blocking(dashboard_root, &expected_admission.scope)
            .expect("pending cleanup authority");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].path, journal_path);
    assert!(
        tracedecay_agent_hosts::automation::scheduler::AutomationTaskLock::try_acquire_keyed(
            dashboard_root,
            &format!("user_job_{job_id}"),
            None,
            now_secs(),
        )
        .await
        .expect("post-terminal lock")
        .is_some()
    );

    let journal_before_recovery = std::fs::read(&journal_path).expect("terminal journal bytes");
    let terminal_path = terminal_sidecar_path(&journal_path).expect("terminal sidecar path");
    let terminal_before_recovery = std::fs::read(&terminal_path).expect("terminal sidecar bytes");
    let ledger_path =
        tracedecay_agent_hosts::automation::run_ledger::run_ledger_path(dashboard_root);
    let ledger_before_recovery = std::fs::read(&ledger_path).expect("exact ledger bytes");
    let recovery = recovery_index::reconcile_reserved_automation_effects_for_project(
        &cg,
        dashboard_root,
        &tracedecay_application::CancellationSignal::active("cancellation.corrupt-spool-recovery")
            .expect("recovery cancellation"),
    )
    .await
    .expect("canonical corrupt spool recovery");
    assert_eq!(recovery.inspected, 1);
    assert_eq!(recovery.already_terminal, 1);
    assert_eq!(exact_spool_file_count(dashboard_root), 0);
    assert!(
        recovery_index::indexed_journals_blocking(dashboard_root, &expected_admission.scope)
            .expect("post-recovery pending authority")
            .is_empty()
    );
    assert_eq!(
        std::fs::read(&journal_path).expect("recovered terminal journal"),
        journal_before_recovery
    );
    assert_eq!(
        std::fs::read(&terminal_path).expect("recovered terminal sidecar"),
        terminal_before_recovery
    );
    assert_eq!(
        std::fs::read(&ledger_path).expect("recovered exact ledger"),
        ledger_before_recovery
    );
    assert_eq!(
        tracedecay_agent_hosts::automation::run_ledger::find_run_record_exact_bounded_blocking(
            dashboard_root,
            run_id,
        )
        .expect("exact lookup after canonical recovery"),
        Some(expected_record)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retained_projector_panic_finishes_recovery_before_releasing_task_lock() {
    use fs2::FileExt;
    use std::time::{Duration, Instant};

    let temp = tempfile::tempdir().expect("tempdir");
    let dashboard_root = temp.path();
    let run_id = "run.retained-projector-panic";
    let job_id = "retained-projector-panic";
    let retained = retained_disabled_user_job_run(dashboard_root, run_id, job_id).await;
    let admission = external_admission_for_job(run_id, "request.retained-projector-panic", job_id);
    let (authority, journal_path, expected_admission) =
        retained_external_authority(dashboard_root, admission);
    recovery_index::add_pending_blocking(dashboard_root, &journal_path, &expected_admission)
        .expect("retain projector-panic authority");
    let reserved = read_indexed_record_blocking(&journal_path)
        .expect("projector-panic reserved journal read")
        .expect("projector-panic reserved journal");
    assert!(!reserved.is_terminal());
    assert_eq!(reserved.admission(), &expected_admission);

    let journal_lock_path = crate::storage::append_lock_path(&journal_path);
    let journal_lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&journal_lock_path)
        .expect("projector-panic journal lock");
    journal_lock
        .lock_exclusive()
        .expect("block projector-panic recovery terminal");

    let (projected_tx, projected_rx) = std::sync::mpsc::channel();
    let waiter = authority.start_retained_automation_settlement(
        retained,
        None,
        move |_| -> (
            AutomationRunLedgerRecord,
            Option<AutomationCommittedReceipt>,
        ) {
            projected_tx
                .send(())
                .expect("signal projector execution inside owner");
            panic!("injected retained settlement projector panic");
        },
    );
    drop(waiter);
    projected_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("detached owner executed projector");

    assert!(task_lock_is_denied(dashboard_root, job_id).await);

    FileExt::unlock(&journal_lock).expect("release projector-panic recovery terminal");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let terminal = read_indexed_record_blocking(&journal_path)
            .expect("projector-panic terminal journal read")
            .is_some_and(|record| record.is_terminal());
        if terminal {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "projector-panic recovery terminal did not become durable"
        );
        tokio::task::yield_now().await;
    }
    assert!(
        tracedecay_agent_hosts::automation::run_ledger::find_run_record_exact_bounded_blocking(
            dashboard_root,
            run_id,
        )
        .expect("projector-panic exact lookup")
        .is_none(),
        "projector panic must not fabricate a successful ledger row"
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if tracedecay_agent_hosts::automation::scheduler::AutomationTaskLock::try_acquire_keyed(
            dashboard_root,
            &format!("user_job_{job_id}"),
            None,
            now_secs(),
        )
        .await
        .expect("post-projector-panic task lock")
        .is_some()
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "projector-panic task lock was not released after durable recovery"
        );
        tokio::task::yield_now().await;
    }
}

struct RequestWaitingPairFixture {
    submission: Option<super::super::DeferredSettlementPairSubmission<()>>,
    journal_paths: [std::path::PathBuf; 2],
    admissions: [DurableAutomationAdmission; 2],
    run_ids: [String; 2],
    job_ids: [String; 2],
}

async fn request_waiting_pair_fixture(
    dashboard_root: &std::path::Path,
    label: &str,
) -> RequestWaitingPairFixture {
    let run_ids = [format!("run.{label}.first"), format!("run.{label}.second")];
    let job_ids = [format!("{label}-first"), format!("{label}-second")];
    let (_, first_guard) =
        retained_disabled_user_job(dashboard_root, &run_ids[0], &job_ids[0]).await;
    let (_, second_guard) =
        retained_disabled_user_job(dashboard_root, &run_ids[1], &job_ids[1]).await;
    let (first_authority, first_journal, first_admission) = retained_external_authority(
        dashboard_root,
        external_admission_for_job(&run_ids[0], &format!("request.{label}.first"), &job_ids[0]),
    );
    let (second_authority, second_journal, second_admission) = retained_external_authority(
        dashboard_root,
        external_admission_for_job(&run_ids[1], &format!("request.{label}.second"), &job_ids[1]),
    );
    recovery_index::add_pending_blocking(dashboard_root, &first_journal, &first_admission)
        .expect("retain first request-waiting pair authority");
    recovery_index::add_pending_blocking(dashboard_root, &second_journal, &second_admission)
        .expect("retain second request-waiting pair authority");
    let submission =
        super::super::AutomationEffectAuthority::start_request_waiting_settlement_pair_with_phase_hooks(
            (first_authority, first_guard),
            (second_authority, second_guard),
            None,
            None,
        );
    RequestWaitingPairFixture {
        submission: Some(submission),
        journal_paths: [first_journal, second_journal],
        admissions: [first_admission, second_admission],
        run_ids,
        job_ids,
    }
}

async fn assert_request_waiting_pair_abandoned_cleanly(
    dashboard_root: &std::path::Path,
    fixture: &RequestWaitingPairFixture,
) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let journals_absent = fixture.journal_paths.iter().all(|path| !path.exists());
        let pending_absent = fixture.admissions.iter().all(|admission| {
            recovery_index::indexed_journals_blocking(dashboard_root, &admission.scope)
                .is_ok_and(|pending| pending.is_empty())
        });
        let locks_released = !task_lock_is_denied(dashboard_root, &fixture.job_ids[0]).await
            && !task_lock_is_denied(dashboard_root, &fixture.job_ids[1]).await;
        if journals_absent && pending_absent && locks_released {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "request-waiting pair did not abandon both authorities before releasing its locks"
        );
        tokio::task::yield_now().await;
    }
    for (journal_path, admission) in fixture.journal_paths.iter().zip(&fixture.admissions) {
        assert!(!journal_path.exists(), "abandoned journal remained present");
        assert!(
            !terminal_sidecar_path(journal_path)
                .expect("abandoned terminal sidecar path")
                .exists(),
            "abandonment fabricated a terminal sidecar"
        );
        assert!(
            recovery_index::indexed_journals_blocking(dashboard_root, &admission.scope)
                .expect("post-abandonment recovery index")
                .is_empty(),
            "abandoned pair left recovery-index debris"
        );
    }
    assert!(
        exact_spool_files(dashboard_root).is_empty(),
        "abandoned pair left exact-publication spool debris"
    );
    for run_id in &fixture.run_ids {
        assert!(
            tracedecay_agent_hosts::automation::run_ledger::find_run_record_exact_bounded_blocking(
                dashboard_root,
                run_id,
            )
            .expect("abandoned pair exact ledger lookup")
            .is_none(),
            "abandoned pair fabricated an exact ledger row"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropping_request_waiting_pair_before_submit_abandons_both_authorities() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dashboard_root = temp.path();
    let mut fixture = request_waiting_pair_fixture(dashboard_root, "pair-launch-drop").await;
    assert!(task_lock_is_denied(dashboard_root, &fixture.job_ids[0]).await);
    assert!(task_lock_is_denied(dashboard_root, &fixture.job_ids[1]).await);
    drop(
        fixture
            .submission
            .take()
            .expect("request-waiting pair submission"),
    );
    assert_request_waiting_pair_abandoned_cleanly(dashboard_root, &fixture).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn partial_pair_submit_abandons_the_closed_sibling_under_shared_guard_ownership() {
    use fs2::FileExt;

    let temp = tempfile::tempdir().expect("tempdir");
    let dashboard_root = temp.path();
    let mut fixture = request_waiting_pair_fixture(dashboard_root, "pair-partial-submit").await;
    let second_journal_lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(crate::storage::append_lock_path(&fixture.journal_paths[1]))
        .expect("open second request-waiting journal lock");
    second_journal_lock
        .lock_exclusive()
        .expect("block closed sibling abandonment");
    let submission = fixture
        .submission
        .take()
        .expect("request-waiting pair submission");
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _waiter = submission
            .submit_with_hook(
                super::super::DeferredSettlementRequest::Abandon,
                super::super::DeferredSettlementRequest::Abandon,
                || panic!("injected unwind after first pair request submission"),
            )
            .expect("unreachable pair submission result");
    }));
    assert!(unwind.is_err(), "partial-submit fixture did not unwind");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while fixture.journal_paths[0].exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "submitted first abandonment did not finish"
        );
        tokio::task::yield_now().await;
    }
    assert!(
        task_lock_is_denied(dashboard_root, &fixture.job_ids[0]).await,
        "finished first leg released its lock before the closed sibling finished"
    );
    assert!(
        task_lock_is_denied(dashboard_root, &fixture.job_ids[1]).await,
        "closed sibling released its lock before durable abandonment"
    );
    FileExt::unlock(&second_journal_lock).expect("release closed sibling abandonment");
    assert_request_waiting_pair_abandoned_cleanly(dashboard_root, &fixture).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn panic_before_pair_projection_closes_both_request_channels_and_abandons() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dashboard_root = temp.path();
    let mut fixture = request_waiting_pair_fixture(dashboard_root, "pair-projection-panic").await;
    let mut submission = fixture
        .submission
        .take()
        .expect("request-waiting pair submission");
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        submission
            .take_payload()
            .expect("take retained pair dispatch payload");
        panic!("injected unwind before projecting pair settlement requests");
    }));
    assert!(unwind.is_err(), "pre-projection fixture did not unwind");
    assert_request_waiting_pair_abandoned_cleanly(dashboard_root, &fixture).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retained_pair_attempts_second_leg_and_keeps_both_guards_until_both_finish() {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    let temp = tempfile::tempdir().expect("tempdir");
    let dashboard_root = temp.path();
    let first_run_id = "run.retained-pair-first";
    let second_run_id = "run.retained-pair-second";
    let first_job_id = "retained-pair-first";
    let second_job_id = "retained-pair-second";
    let (mut first_run, first_guard) =
        retained_disabled_user_job(dashboard_root, first_run_id, first_job_id).await;
    let (second_run, second_guard) =
        retained_disabled_user_job(dashboard_root, second_run_id, second_job_id).await;
    let second_expected_record = second_run.ledger_record.clone();
    let (first_authority, first_journal, first_admission) = retained_external_authority(
        dashboard_root,
        external_admission_for_job(first_run_id, "request.retained-pair-first", first_job_id),
    );
    let (second_authority, second_journal, second_admission) = retained_external_authority(
        dashboard_root,
        external_admission_for_job(second_run_id, "request.retained-pair-second", second_job_id),
    );

    first_run.ledger_record.run_id = "run.invalid-pair-projection".to_owned();
    let (phase_tx, phase_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let phase_release = Arc::clone(&release_rx);
    let second_phase_hook = super::super::SettlementPhaseHook::new(move |phase| {
        if phase == super::super::RetainedSettlementPhase::Prepared {
            phase_tx.send(phase).expect("second leg prepared");
            phase_release
                .lock()
                .expect("pair phase release lock")
                .recv()
                .expect("release second pair leg");
        }
    });
    let (second_observed_tx, second_observed_rx) = std::sync::mpsc::channel();
    let submission =
        super::super::AutomationEffectAuthority::start_request_waiting_settlement_pair_with_phase_hooks(
            (first_authority, first_guard),
            (second_authority, second_guard),
            None,
            Some(second_phase_hook),
        );
    let waiter = submission
        .submit(
            super::super::DeferredSettlementRequest::Run(Box::new(
                super::super::DeferredRunSettlementRequest {
                    ledger: first_run.ledger_record,
                    committed: first_run.committed_receipt,
                    observer: None,
                },
            )),
            super::super::DeferredSettlementRequest::Run(Box::new(
                super::super::DeferredRunSettlementRequest {
                    ledger: second_run.ledger_record,
                    committed: second_run.committed_receipt,
                    observer: Some(Box::new(move |record| {
                        second_observed_tx
                            .send(record.clone())
                            .expect("second exact observation");
                    })),
                },
            )),
        )
        .expect("submit both pair terminals");

    assert_eq!(
        phase_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("second pair leg attempted"),
        super::super::RetainedSettlementPhase::Prepared
    );
    let super::super::RetainedSettlementPairWaiter { first, second, .. } = waiter;
    let first_result = tokio::time::timeout(Duration::from_secs(5), first.wait())
        .await
        .expect("first pair owner must finish while second remains paused");
    assert!(
        first_result.is_err(),
        "invalid first projection must preserve its typed error after durable fallback"
    );
    let first_terminal = read_indexed_record_blocking(&first_journal)
        .expect("first pair journal read")
        .expect("first pair journal");
    assert!(first_terminal.is_terminal());
    assert_eq!(first_terminal.admission(), &first_admission);
    assert!(task_lock_is_denied(dashboard_root, first_job_id).await);
    assert!(task_lock_is_denied(dashboard_root, second_job_id).await);
    assert!(
        tracedecay_agent_hosts::automation::run_ledger::find_run_record_exact_bounded_blocking(
            dashboard_root,
            first_run_id,
        )
        .expect("first recovery exact lookup")
        .is_none()
    );

    release_tx.send(()).expect("release second pair leg");
    let second_owned = tokio::time::timeout(Duration::from_secs(5), second.wait())
        .await
        .expect("second pair owner must finish")
        .expect("second pair settlement");
    let super::super::DeferredSettlementOutcome::Settled(second_outcome) = &second_owned.outcome
    else {
        panic!("second pair leg must settle its exact terminal")
    };
    assert_eq!(
        second_outcome
            .terminal
            .run_result()
            .expect("second pair run result")
            .run_id
            .as_str(),
        second_run_id
    );
    assert_eq!(
        second_observed_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("second pair exact observation"),
        second_expected_record.clone()
    );
    assert_eq!(
        tracedecay_agent_hosts::automation::run_ledger::find_run_record_exact_bounded_blocking(
            dashboard_root,
            second_run_id,
        )
        .expect("second pair exact lookup"),
        Some(second_expected_record)
    );
    let second_terminal = read_indexed_record_blocking(&second_journal)
        .expect("second pair terminal read")
        .expect("second pair journal");
    assert!(second_terminal.is_terminal());
    assert_eq!(second_terminal.admission(), &second_admission);
    assert!(task_lock_is_denied(dashboard_root, first_job_id).await);
    assert!(task_lock_is_denied(dashboard_root, second_job_id).await);

    drop(second_owned);
    for job_id in [first_job_id, second_job_id] {
        let reacquired =
            tracedecay_agent_hosts::automation::scheduler::AutomationTaskLock::try_acquire_keyed(
                dashboard_root,
                &format!("user_job_{job_id}"),
                None,
                now_secs(),
            )
            .await
            .expect("post-pair lock probe");
        assert!(
            reacquired.is_some(),
            "pair guard for {job_id} was not released"
        );
    }
}

#[test]
fn durable_abandonment_is_idempotent_after_parent_sync() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("automation_effects").join("abandon.json");
    let admission = external_admission("run.abandon-idempotent", "request.abandon-idempotent");
    let claim = match reserve_or_replay_blocking(&path, admission.clone()).expect("reserve") {
        ReservationResult::Execute { claim, .. } => claim,
        _ => panic!("fresh admission must execute"),
    };
    abandon_reservation_blocking(&path, &admission).expect("first durable abandon");
    abandon_reservation_blocking(&path, &admission).expect("idempotent durable abandon");
    assert!(!path.exists());
    drop(claim);
}

#[test]
fn durable_settlement_classifier_requires_terminal_before_release() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("classified-settlement.json");
    let admission = admission("run.classified-settlement", "request.classified-settlement");
    let terminal = success_terminal(&admission, "run.classified-settlement");
    let publication = exact_publication('f', 8_192);
    let claim = reserve_or_replay_blocking(&path, admission.clone()).expect("reserve");

    assert_eq!(
        classify_durable_settlement_blocking(&path, &admission, &terminal, Some(&publication))
            .expect("reserved classification"),
        DurableSettlementClassification::Reserved
    );
    persist_prepared_terminal_blocking(&path, &admission, &terminal, publication.clone())
        .expect("prepare terminal");
    assert_eq!(
        classify_durable_settlement_blocking(&path, &admission, &terminal, Some(&publication))
            .expect("prepared classification"),
        DurableSettlementClassification::Prepared
    );
    promote_prepared_terminal_blocking(&path, &admission, terminal.clone(), &publication)
        .expect("promote terminal");
    assert_eq!(
        classify_durable_settlement_blocking(&path, &admission, &terminal, Some(&publication))
            .expect("terminal classification"),
        DurableSettlementClassification::Terminal
    );
    drop(claim);
}

/// A `PreparedWriteHook` that always fails never lets `settle_bound_once`
/// reach a terminal classification, so the blocking-pool retry loop in
/// `settle_bound_owner` used to spin forever. With a bounded retry budget
/// it must instead resolve with an error that names the exceeded budget,
/// while leaving the journal in its recoverable Reserved state.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retained_settlement_exceeds_retry_budget_instead_of_hanging_forever() {
    use std::time::Duration;

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
    .expect("initialize retry-budget fixture project");
    let dashboard_root = &cg.store_layout().dashboard_root;
    let run_id = "run.retry-budget-exhausted";
    let job_id = "retry-budget-exhausted";
    let (run, guard) = retained_disabled_user_job(dashboard_root, run_id, job_id).await;

    let mut cleanup_admission =
        external_admission_for_job(run_id, "request.retry-budget-exhausted", job_id);
    cleanup_admission.recovery = AutomationRecoveryBinding::External {
        recovery_problem: reset_problem(
            &cleanup_admission.request_id,
            &cleanup_admission.scope,
            &cleanup_admission.request,
        ),
    };
    let cleanup_admission = seal_effect_authority(cleanup_admission);
    let (authority, journal_path, expected_admission) =
        retained_external_authority(dashboard_root, cleanup_admission);
    recovery_index::add_pending_blocking(dashboard_root, &journal_path, &expected_admission)
        .expect("retain retry-budget cleanup authority");

    let terminal = authority
        .terminal_for_run(&run.ledger_record, run.committed_receipt.as_ref())
        .expect("terminal for retry-budget fixture");
    let always_fails_write_hook = super::super::PreparedWriteHook::new(|_publication| {
        Err(super::super::contract_error(
            "injected prepared journal write failure (always fails, for retry-budget test)",
        ))
    });
    let state = super::super::RetainedBoundSettlement {
        authority,
        guard: super::super::RetainedSettlementGuardOwner::Single(guard),
        terminal,
        ledger: run.ledger_record,
        publication: None,
        observer: None,
        phase_hook: None,
        prepared_write_hook: Some(always_fails_write_hook),
    };

    let waiter = super::super::RetainedSettlementWaiter {
        task: tokio::task::spawn_blocking(move || {
            super::super::settle_bound_owner_with_budget(state, Duration::from_millis(200))
                .map(|owned| owned.value)
        }),
    };
    let error = waiter.wait().await.expect_err(
        "settlement must resolve with an error, not hang, once its retry budget is exhausted",
    );
    let message = error.to_string();
    assert!(
        message.contains("retry budget"),
        "expected the error to name the exceeded retry budget, got: {message}"
    );

    let reserved = read_indexed_record_blocking(&journal_path)
        .expect("reserved journal read after retry-budget exhaustion")
        .expect("reserved journal remains present");
    assert!(
        !reserved.is_terminal(),
        "budget exhaustion must not fabricate a terminal; recovery relies on the Reserved state"
    );
    assert_eq!(reserved.admission(), &expected_admission);
}
