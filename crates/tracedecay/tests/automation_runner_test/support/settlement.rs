//! Runs the production retained automation runners and settles their
//! terminals through the durable effect authority, the way the daemon
//! scheduler does, so ledger rows appear only once settlement publishes them.

use std::collections::BTreeSet;
use std::path::Path;

use tracedecay_automation_runtime::automation::backend::AgentTaskBackend;
use tracedecay_automation_runtime::automation::combined_effect::{
    DeferredLegTerminal, combined_dispatch_terminals,
};
use tracedecay_automation_runtime::automation::config::AutomationConfig;
use tracedecay_automation_runtime::automation::effect_runtime::settlement::{
    AdmittedAutomationEffectRequest, AutomationEffectAdmission, AutomationEffectAuthority,
    DeferredProblemSettlementRequest, DeferredRunSettlementRequest, DeferredSettlementRequest,
};
use tracedecay_automation_runtime::automation::effect_runtime::{
    contract_error, memory_curator_run_request, session_reflector_run_request,
    skill_writer_run_request, user_job_run_request,
};
use tracedecay_automation_runtime::automation::jobs::{
    AutomationJob, UserJobAutomationRun, UserJobRunOptions,
    run_user_job_with_backend_for_retained_settlement,
};
use tracedecay_automation_runtime::automation::run_ledger::{
    AutomationRunLedgerRecord, AutomationRunStatus,
};
use tracedecay_automation_runtime::automation::runner::{
    AutomationRunSettlementGuard, AutomationSessionRetrieval, CombinedFailureTerminals,
    CombinedMemoryCompletedSkillFailure, CombinedRecordedFailure, CombinedReflectorPartial,
    CombinedReviewAutomationOptions, CombinedReviewDispatch, CombinedSkillPartial,
    MemoryCuratorAutomationOptions, MemoryCuratorAutomationRun, RetainedAutomationRun,
    RetainedAutomationSettlementDisposition, SessionReflectorAutomationOptions,
    SessionReflectorAutomationRun, SkillWriterAutomationOptions, SkillWriterAutomationRun,
    run_combined_review_with_backend_and_retrieval_for_retained_settlement,
    run_memory_curator_with_backend_for_retained_settlement,
    run_session_reflector_with_backend_and_retrieval_for_retained_settlement,
    run_skill_writer_with_backend_and_retrieval_for_retained_settlement,
};
use tracedecay_automation_runtime::automation::{
    AutomationCommittedReceipt, AutomationRunControl, AutomationRunError, AutomationRunResult,
};
use tracedecay_automation_runtime::ports::project_runtime::AutomationProjectContext;
use tracedecay_contracts::retained_surfaces::{
    AutomationRunRequestV1, RetainedSurfaceOperation, retained_surface_application_operation,
};
use tracedecay_contracts::{
    CancellationContext, CancellationSignal, CapabilityGrantId, CapabilityGrantSnapshot, Deadline,
    DisclosureClass, RequestContext, ResolvedScope,
};
use tracedecay_daemon_service::automation_effect::scheduler_automation_request_id;
use tracedecay_domain::configuration::ConfigurationRevisionId;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_domain::{
    ActorId, FactOwnerV1, ProjectId, RepositoryId, UtcMicros, WorktreeId, canonical_sha256,
};

/// Admits one retained automation request the way the daemon's composition
/// root does, with a fixture grant standing in for the registered project
/// runtime. Memory-owning runs are scoped to their project's memory owner.
async fn admit(
    dashboard_root: &Path,
    project_id: Option<&ProjectId>,
    request: AutomationRunRequestV1,
) -> AutomationEffectAuthority {
    let operation =
        retained_surface_application_operation(RetainedSurfaceOperation::FactStoreCurate)
            .expect("retained operation");
    let owner = project_id.map(|project_id| FactOwnerV1::Project {
        project_id: project_id.clone(),
    });
    let scope = ResolvedScope::new(
        project_id
            .cloned()
            .unwrap_or_else(|| ProjectId::new("project.automation-runner-test").expect("project")),
        RepositoryId::new("repository.automation-runner-test").expect("repository"),
        WorktreeId::new("worktree.automation-runner-test").expect("worktree"),
        None,
    )
    .expect("scope");
    let actor = ActorId::new("actor.automation-runner-test").expect("actor");
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.automation-runner-test").expect("grant id"),
        1,
        canonical_sha256(&"automation-runner-test.grant").expect("grant digest"),
        actor.clone(),
        UtcMicros(1),
        UtcMicros(i64::MAX - 1),
        scope.clone(),
        BTreeSet::from([operation.capability_id().clone()]),
        BTreeSet::from([operation.use_case_id().clone()]),
        DisclosureClass::Evidence,
    )
    .expect("grant");
    let request_id =
        scheduler_automation_request_id(Some(request.run_id.as_str())).expect("request id");
    let cancellation_id = format!("cancel.{}", request_id.as_str());
    let context = RequestContext::new(
        actor,
        scope,
        grant,
        request_id,
        Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
        CancellationContext::active(cancellation_id.clone()).expect("cancellation context"),
    )
    .expect("request context");
    let admission = Box::pin(AutomationEffectAuthority::prepare(
        AdmittedAutomationEffectRequest {
            context,
            cancellation: CancellationSignal::active(cancellation_id).expect("cancellation"),
            observed_at: tracedecay_contracts::now_micros(),
            configuration_digest: canonical_sha256(&"automation-runner-test.configuration")
                .expect("configuration digest"),
            request,
            dashboard_root: dashboard_root.to_path_buf(),
        },
        move || owner.ok_or_else(|| contract_error("fixture run has no memory owner")),
        |_, _| async { Err(contract_error("fresh admission must not recover receipts")) },
    ))
    .await
    .expect("durable automation admission");
    match admission {
        AutomationEffectAdmission::Execute(authority) => *authority,
        AutomationEffectAdmission::Replay(_) => panic!("run id was already settled"),
        AutomationEffectAdmission::Conflict => panic!("run id conflicts with a durable admission"),
        AutomationEffectAdmission::PreAdmissionProblem(problem) => {
            panic!("automation admission was refused: {problem:?}")
        }
    }
}

fn admitted_run_id(requested: Option<String>) -> String {
    requested.unwrap_or_else(|| {
        scheduler_automation_request_id(None)
            .expect("minted run id")
            .as_str()
            .to_owned()
    })
}

trait SettledRun {
    fn ledger_record(&self) -> &AutomationRunLedgerRecord;
    fn committed_receipt(&self) -> Option<&AutomationCommittedReceipt>;
}

macro_rules! settled_run {
    ($($run:ty),*) => {$(
        impl SettledRun for $run {
            fn ledger_record(&self) -> &AutomationRunLedgerRecord {
                &self.ledger_record
            }
            fn committed_receipt(&self) -> Option<&AutomationCommittedReceipt> {
                self.committed_receipt.as_ref()
            }
        }
    )*};
}

settled_run!(
    MemoryCuratorAutomationRun,
    SessionReflectorAutomationRun,
    SkillWriterAutomationRun,
    UserJobAutomationRun
);

/// Reproduces a runner error for settlement while the original goes back to
/// the test. Settlement reads only the ledger record, the committed receipt,
/// and whether the error demands a reset.
fn error_copy(error: &TraceDecayError) -> TraceDecayError {
    match error.reset_required_context() {
        Some((authority, reason)) => TraceDecayError::reset_required(authority, reason),
        None => TraceDecayError::Config {
            message: error.to_string(),
        },
    }
}

fn run_error_copy(error: &AutomationRunError) -> AutomationRunError {
    match error {
        AutomationRunError::Runtime(error) => AutomationRunError::Runtime(error_copy(error)),
        AutomationRunError::RecordedFailure {
            error,
            ledger_record,
        } => AutomationRunError::RecordedFailure {
            error: error_copy(error),
            ledger_record: ledger_record.clone(),
        },
        AutomationRunError::PartialEffect {
            run_id,
            committed_receipt,
            ledger_record,
            detail,
        } => AutomationRunError::PartialEffect {
            run_id: run_id.clone(),
            committed_receipt: committed_receipt.clone(),
            ledger_record: ledger_record.clone(),
            detail,
        },
    }
}

/// Settles one retained run exactly as the daemon's replay-leg settlement
/// does and returns the runner's own result.
async fn settle<T: SettledRun>(
    authority: AutomationEffectAuthority,
    retained: RetainedAutomationRun<T>,
) -> AutomationRunResult<T> {
    match retained.into_settlement_disposition() {
        RetainedAutomationSettlementDisposition::Current {
            result,
            settlement_guard,
        } => settle_current(authority, result, settlement_guard).await,
        RetainedAutomationSettlementDisposition::ReusedSchedulerSkip { reused, .. } => panic!(
            "scheduler skip reused prior terminal {}; settlement discards the runner result",
            reused.prior_record.run_id
        ),
    }
}

pub(crate) async fn settle_memory_curator(
    cx: &AutomationProjectContext,
    config: &AutomationConfig,
    configuration_revision_id: &ConfigurationRevisionId,
    backend: &dyn AgentTaskBackend,
    mut options: MemoryCuratorAutomationOptions,
    run_control: &AutomationRunControl,
) -> AutomationRunResult<MemoryCuratorAutomationRun> {
    let run_id = admitted_run_id(options.run_id.take());
    let request =
        memory_curator_run_request(&run_id, options.fact_review_limit, options.min_confidence)
            .expect("memory curator request");
    let authority = admit(&cx.dashboard_root, Some(cx.project_id()), request).await;
    options.run_id = Some(run_id);
    let retained = run_memory_curator_with_backend_for_retained_settlement(
        cx,
        config,
        configuration_revision_id,
        backend,
        options,
        run_control,
    )
    .await;
    settle(authority, retained).await
}

pub(crate) async fn settle_session_reflector(
    cx: &AutomationProjectContext,
    config: &AutomationConfig,
    run_control: &AutomationRunControl,
    configuration_revision_id: &ConfigurationRevisionId,
    backend: &dyn AgentTaskBackend,
    retrieval: &dyn AutomationSessionRetrieval,
    mut options: SessionReflectorAutomationOptions,
) -> AutomationRunResult<SessionReflectorAutomationRun> {
    let run_id = admitted_run_id(options.run_id.take());
    let request =
        session_reflector_run_request(&run_id, &options).expect("session reflector request");
    let authority = admit(&cx.dashboard_root, Some(cx.project_id()), request).await;
    options.run_id = Some(run_id);
    let retained = run_session_reflector_with_backend_and_retrieval_for_retained_settlement(
        cx,
        config,
        run_control,
        configuration_revision_id,
        backend,
        retrieval,
        options,
    )
    .await;
    settle(authority, retained).await
}

pub(crate) async fn settle_skill_writer(
    cx: &AutomationProjectContext,
    config: &AutomationConfig,
    configuration_revision_id: &ConfigurationRevisionId,
    backend: &dyn AgentTaskBackend,
    retrieval: &dyn AutomationSessionRetrieval,
    mut options: SkillWriterAutomationOptions,
) -> AutomationRunResult<SkillWriterAutomationRun> {
    let run_id = admitted_run_id(options.run_id.take());
    let request = skill_writer_run_request(&run_id, &options).expect("skill writer request");
    let authority = admit(&cx.dashboard_root, Some(cx.project_id()), request).await;
    options.run_id = Some(run_id);
    let retained = run_skill_writer_with_backend_and_retrieval_for_retained_settlement(
        cx,
        config,
        configuration_revision_id,
        backend,
        retrieval,
        options,
    )
    .await;
    settle(authority, retained).await
}

/// Runs one user job and settles it the way the scheduler does: a
/// scheduler-lock diagnostic was already published under its own identity,
/// so the admitted reservation is abandoned instead of settled.
pub(crate) async fn settle_user_job(
    dashboard_root: &Path,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    job: &AutomationJob,
    mut options: UserJobRunOptions,
) -> AutomationRunResult<UserJobAutomationRun> {
    let run_id = admitted_run_id(options.run_id.take());
    let request = user_job_run_request(&run_id, &job.id).expect("user job request");
    let authority = admit(dashboard_root, None, request).await;
    options.run_id = Some(run_id);
    let retained = run_user_job_with_backend_for_retained_settlement(
        dashboard_root,
        config,
        backend,
        job,
        options,
    )
    .await;
    let RetainedAutomationSettlementDisposition::Current {
        result,
        settlement_guard,
    } = retained.into_settlement_disposition()
    else {
        panic!("user jobs never reuse a fixed-task scheduler skip")
    };
    if let Ok(run) = &result
        && run.ledger_record.status == AutomationRunStatus::Skipped
        && run.ledger_record.error.as_deref() == Some("scheduler_lock_active")
    {
        authority
            .abandon_uncommitted()
            .await
            .expect("abandon scheduler-lock reservation");
        drop(settlement_guard);
        return result;
    }
    settle_current(authority, result, settlement_guard).await
}

async fn settle_current<T: SettledRun>(
    authority: AutomationEffectAuthority,
    result: AutomationRunResult<T>,
    settlement_guard: AutomationRunSettlementGuard,
) -> AutomationRunResult<T> {
    match &result {
        Ok(run) => {
            authority
                .start_deferred_run_settlement_observed(
                    run.ledger_record().clone(),
                    run.committed_receipt().cloned(),
                    settlement_guard,
                    None,
                )
                .wait()
                .await
                .expect("run settlement");
        }
        Err(error) => {
            authority
                .start_deferred_problem_settlement_observed(
                    run_error_copy(error),
                    settlement_guard,
                    None,
                )
                .wait()
                .await
                .expect("problem settlement");
        }
    }
    result
}

fn dispatch_copy(dispatch: &CombinedReviewDispatch) -> CombinedReviewDispatch {
    let errors = |error: &Option<TraceDecayError>| error.as_ref().map(error_copy);
    match dispatch {
        CombinedReviewDispatch::Ran(run) => CombinedReviewDispatch::Ran(run.clone()),
        CombinedReviewDispatch::MemoryCompletedSkillFailure(failure) => {
            CombinedReviewDispatch::MemoryCompletedSkillFailure(Box::new(
                CombinedMemoryCompletedSkillFailure {
                    session_reflector: failure.session_reflector.clone(),
                    skill_writer_record: failure.skill_writer_record.clone(),
                    skill_writer_record_error: errors(&failure.skill_writer_record_error),
                    error: error_copy(&failure.error),
                },
            ))
        }
        CombinedReviewDispatch::RecordedFailure(failure) => {
            CombinedReviewDispatch::RecordedFailure(Box::new(CombinedRecordedFailure {
                run: failure.run.clone(),
                error: error_copy(&failure.error),
            }))
        }
        CombinedReviewDispatch::FailureTerminals(failure) => {
            CombinedReviewDispatch::FailureTerminals(Box::new(CombinedFailureTerminals {
                reflector_record: failure.reflector_record.clone(),
                reflector_error: errors(&failure.reflector_error),
                skill_writer_record: failure.skill_writer_record.clone(),
                skill_writer_error: errors(&failure.skill_writer_error),
                error: error_copy(&failure.error),
            }))
        }
        CombinedReviewDispatch::ReflectorPartial(partial) => {
            CombinedReviewDispatch::ReflectorPartial(Box::new(CombinedReflectorPartial {
                run_id: partial.run_id.clone(),
                committed_receipt: partial.committed_receipt.clone(),
                ledger_record: partial.ledger_record.clone(),
                reflector_record_error: errors(&partial.reflector_record_error),
                skill_writer_record: partial.skill_writer_record.clone(),
                skill_writer_error: errors(&partial.skill_writer_error),
                detail: partial.detail,
            }))
        }
        CombinedReviewDispatch::SkillPartial(partial) => {
            CombinedReviewDispatch::SkillPartial(Box::new(CombinedSkillPartial {
                completed_session_reflector: partial.completed_session_reflector.clone(),
                run_id: partial.run_id.clone(),
                committed_receipt: partial.committed_receipt.clone(),
                ledger_record: partial.ledger_record.clone(),
                skill_writer_record_error: errors(&partial.skill_writer_record_error),
                detail: partial.detail,
            }))
        }
        CombinedReviewDispatch::NotCombined { reason } => {
            CombinedReviewDispatch::NotCombined { reason }
        }
    }
}

fn deferred_request(terminal: DeferredLegTerminal) -> DeferredSettlementRequest {
    match terminal {
        DeferredLegTerminal::Run(terminal) => {
            DeferredSettlementRequest::Run(Box::new(DeferredRunSettlementRequest {
                ledger: terminal.record,
                committed: terminal.committed,
                observer: None,
            }))
        }
        DeferredLegTerminal::Problem(terminal) => {
            DeferredSettlementRequest::Problem(Box::new(DeferredProblemSettlementRequest {
                error: terminal.error,
                observer: None,
            }))
        }
        DeferredLegTerminal::Abandon => DeferredSettlementRequest::Abandon,
    }
}

/// Admits both combined-review legs and settles them as one pair, the way
/// the daemon's combined scheduler effect does.
pub(crate) async fn settle_combined_review(
    cx: &AutomationProjectContext,
    config: &AutomationConfig,
    configuration_revision_id: &ConfigurationRevisionId,
    backend: &dyn AgentTaskBackend,
    retrieval: &dyn AutomationSessionRetrieval,
    mut options: CombinedReviewAutomationOptions,
    run_control: &AutomationRunControl,
) -> Result<CombinedReviewDispatch> {
    let run_id = admitted_run_id(options.run_id.take());
    let reflector_request = session_reflector_run_request(&run_id, &options.session_reflector)
        .expect("session reflector request");
    let skill_request =
        skill_writer_run_request(&format!("{run_id}_skills"), &options.skill_writer)
            .expect("skill writer request");
    let reflector = admit(&cx.dashboard_root, Some(cx.project_id()), reflector_request).await;
    let skill = admit(&cx.dashboard_root, Some(cx.project_id()), skill_request).await;
    options.run_id = Some(run_id);
    let retained = run_combined_review_with_backend_and_retrieval_for_retained_settlement(
        cx,
        config,
        configuration_revision_id,
        backend,
        retrieval,
        options,
        run_control,
    )
    .await;
    let mut settlement = AutomationEffectAuthority::start_retained_combined_settlement_pair(
        retained, reflector, skill,
    );
    let result = settlement.take_payload().expect("combined payload");
    let copy = match &result {
        Ok(dispatch) => Ok(dispatch_copy(dispatch)),
        Err(error) => Err(error_copy(error)),
    };
    let (reflector_terminal, skill_terminal, _, _) = combined_dispatch_terminals(copy, |_, _| {});
    let (reflector_settled, skill_settled) = settlement
        .submit(
            deferred_request(reflector_terminal),
            deferred_request(skill_terminal),
        )
        .expect("submit combined settlement")
        .wait()
        .await;
    reflector_settled.expect("reflector settlement");
    skill_settled.expect("skill writer settlement");
    result
}
