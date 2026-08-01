#[cfg(test)]
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[cfg(test)]
use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestContext, RequestId,
};
use tracedecay_domain::FactOwnerV1;
#[cfg(test)]
use tracedecay_domain::{
    ActorId, ProjectId, RepositoryId, RetrievalGrainV1, SessionId, TemporalCoverageCountsV1,
    UtcMicros, WorktreeId,
};
#[cfg(test)]
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::backend::{
    AgentTaskBackend, AgentTaskKind, AgentTaskRequest, AgentTaskResponse, AgentTaskRetryReport,
    BackendRetryPolicy, agent_task_contract, extract_json_object_prefix, prompt_version,
    run_agent_task_with_retry_report, task_key,
};
use super::config::AutomationConfig;
#[cfg(test)]
use super::fact_proposals::{FactProposalState, record_session_fact_proposals};
use super::lifecycle::{
    AgentRunFinalizer, AgentTaskRunContext, BackendTaskRun, SchedulerGate,
    failed_backend_fallback_report, generated_run_id, task_run_gate, task_skip_reason,
};
use super::run_ledger::{AutomationRunLedgerRecord, AutomationRunStatus, AutomationTrigger};
use super::scheduler::AutomationTaskLock;
use super::skill_writer::{
    activation_policy as skill_writer_activation_policy, validate_and_apply_skill_proposals,
};
#[cfg(test)]
use crate::application::context::{
    BranchId, CancellationToken, CapabilityDigest, ConfigurationDigest, PolicyDigest, ProfileId,
    RequestBudgets, ResolvedGitRoute, ResolvedSessionIdentity, SessionRootId, SessionStoreId,
    session_application_grant_digest,
};
use crate::application::memory::MemoryApplication;
#[cfg(test)]
use crate::application::session::{
    SessionAccess, SessionAuthorizationError, SessionAuthorizationGrant, SessionFreshnessPolicy,
    SessionRequestBinding, SessionRetrievalConfiguration, SessionRetrievalOutcome,
    SessionRetrievalService, SessionScopeAuthorizationRequest, SessionScopeAuthorizer,
    SessionTemporalExecutionPort, SessionTemporalQuery,
};
use crate::errors::{Result, TraceDecayError};
use crate::ports::project_runtime::ProfileRuntime;
#[cfg(test)]
use crate::ports::session_evidence::{LcmGrepSort, LcmScope};
use crate::ports::session_store::AutomationSessionStore;
use crate::store::memory::DatabaseFactStore;
use crate::tracedecay::{TraceDecay, current_timestamp};
use tracedecay_global_db::RegisteredGlobalDb;
#[cfg(test)]
use tracedecay_temporal_query::TemporalKernelResult;
#[cfg(test)]
use tracedecay_temporal_query::context::VersionedTokenEstimator;

mod evidence;
mod retrieval;
mod session_reflector;
mod skill_writer;
#[cfg(test)]
mod user_scope_tests;

use evidence::{
    SessionReflectorEvidenceBundle, SessionReflectorEvidenceOutcome, SkillWriterEvidenceBundle,
    SkillWriterEvidenceOutcome, build_session_reflector_evidence, build_skill_writer_evidence,
    canonical_evidence_hash,
};
use retrieval::{production_project_automation_retrieval, production_user_automation_retrieval};
use session_reflector::{
    ProposedAgentOutput, build_session_reflector_prompt, finalize_session_reflector_success,
};
use skill_writer::{
    build_skill_writer_prompt, rejected_skill_writer_run, skipped_skill_writer_run,
};

#[cfg(test)]
use evidence::{
    AutomationEvidenceFilters, SESSION_REPLAY_SNIPPET_CHARS,
    serialize_automation_temporal_evidence, validate_complete_evidence,
};
#[cfg(test)]
use retrieval::{
    AUTOMATION_SESSION_MAX_BYTES, AutomationWordEstimator, accept_automation_temporal_outcome,
    retrieve_automation_session_evidence,
};
#[cfg(test)]
use session_reflector::{auto_apply_session_fact_proposals, validate_session_fact_proposals};

pub use evidence::{AutomationTemporalEvidence, AutomationTemporalEvidenceItem};
pub use retrieval::registered_project_automation_retrieval;
pub use retrieval::{
    AuthorizedAutomationSessionRetrieval, AutomationSessionRetrieval,
    AutomationSessionRetrievalFuture, AutomationTemporalRetrieval,
};
pub(crate) use session_reflector::run_user_session_reflector_with_backend_and_retrieval;
pub use session_reflector::{
    SessionReflectorAutomationOptions, SessionReflectorAutomationRun,
    run_session_reflector_with_backend, run_session_reflector_with_backend_and_retrieval,
};
pub use skill_writer::{SkillWriterAutomationOptions, SkillWriterAutomationRun};

pub(crate) use super::memory_curator::run_user_memory_curator_with_backend;
pub use super::memory_curator::{
    MemoryCuratorAutomationOptions, MemoryCuratorAutomationRun, run_memory_curator_with_backend,
};

const USER_AUTOMATION_DIR: &str = "user-automation";

/// Profile-level artifact, ledger, and lock root for projectless automation.
pub fn user_automation_root(profile_root: &std::path::Path) -> PathBuf {
    profile_root.join(USER_AUTOMATION_DIR)
}

pub(super) async fn project_automation_sessions(
    cg: &TraceDecay,
) -> Result<Arc<RegisteredGlobalDb>> {
    let FactOwnerV1::Project { project_id } = cg.project_memory_owner()? else {
        return Err(TraceDecayError::Config {
            message: "project automation requires authoritative project session scope".to_string(),
        });
    };
    cg.project_sessions(project_id, vec![cg.store_layout().project_root.clone()])
        .await
}

/// One callable projectless post-session review suitable for host hooks.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UserSessionAutomationOptions {
    #[serde(default)]
    pub session_reflector: SessionReflectorAutomationOptions,
    #[serde(default)]
    pub memory_curator: MemoryCuratorAutomationOptions,
    #[serde(default)]
    pub skill_writer: SkillWriterAutomationOptions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserSessionAutomationRun {
    pub session_reflector: SessionReflectorAutomationRun,
    pub memory_curator: MemoryCuratorAutomationRun,
    pub skill_writer: SkillWriterAutomationRun,
}

pub async fn run_user_session_automation_with_backend(
    profile_root: &std::path::Path,
    session_registry: Arc<dyn ProfileRuntime>,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    options: UserSessionAutomationOptions,
) -> Result<UserSessionAutomationRun> {
    let retrieval = production_user_automation_retrieval(profile_root).await;
    run_user_session_automation_with_backend_and_retrieval(
        profile_root,
        session_registry,
        config,
        backend,
        retrieval.as_ref(),
        options,
    )
    .await
}

pub(crate) async fn run_user_session_automation_with_backend_and_retrieval(
    profile_root: &std::path::Path,
    session_registry: Arc<dyn ProfileRuntime>,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    retrieval: &dyn AutomationSessionRetrieval,
    options: UserSessionAutomationOptions,
) -> Result<UserSessionAutomationRun> {
    let session_reflector = run_user_session_reflector_with_backend_and_retrieval(
        profile_root,
        Arc::clone(&session_registry),
        config,
        backend,
        retrieval,
        options.session_reflector,
    )
    .await?;
    let memory_curator = run_user_memory_curator_with_backend(
        profile_root,
        Arc::clone(&session_registry),
        config,
        backend,
        options.memory_curator,
    )
    .await?;
    let skill_writer = run_user_skill_writer_with_backend_and_retrieval(
        profile_root,
        session_registry,
        config,
        backend,
        retrieval,
        options.skill_writer,
    )
    .await?;
    Ok(UserSessionAutomationRun {
        session_reflector,
        memory_curator,
        skill_writer,
    })
}

pub async fn run_skill_writer_with_backend(
    cg: &TraceDecay,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    options: SkillWriterAutomationOptions,
) -> Result<SkillWriterAutomationRun> {
    let retrieval = production_project_automation_retrieval(cg).await;
    run_skill_writer_with_backend_and_retrieval(cg, config, backend, retrieval.as_ref(), options)
        .await
}

pub async fn run_skill_writer_with_backend_and_retrieval(
    cg: &TraceDecay,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    retrieval: &dyn AutomationSessionRetrieval,
    options: SkillWriterAutomationOptions,
) -> Result<SkillWriterAutomationRun> {
    let sessions_db = project_automation_sessions(cg).await?;
    run_skill_writer_for_store(
        SkillWriterStoreRuntime {
            dashboard_root: cg.store_layout().dashboard_root.clone(),
            sessions_db,
            analytics_project_root: Some(cg.project_root()),
            analytics_db: Some(cg.profile_database().as_ref()),
        },
        retrieval,
        config,
        backend,
        options,
    )
    .await
}

pub(crate) async fn run_user_skill_writer_with_backend_and_retrieval(
    profile_root: &std::path::Path,
    session_registry: Arc<dyn ProfileRuntime>,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    retrieval: &dyn AutomationSessionRetrieval,
    mut options: SkillWriterAutomationOptions,
) -> Result<SkillWriterAutomationRun> {
    options.profile_root = Some(profile_root.to_path_buf());
    let sessions_db = session_registry.profile_sessions().await?;
    run_skill_writer_for_store(
        SkillWriterStoreRuntime {
            dashboard_root: user_automation_root(profile_root),
            sessions_db,
            analytics_project_root: None,
            analytics_db: None,
        },
        retrieval,
        config,
        backend,
        options,
    )
    .await
}

struct SkillWriterStoreRuntime<'a> {
    dashboard_root: PathBuf,
    sessions_db: Arc<RegisteredGlobalDb>,
    analytics_project_root: Option<&'a Path>,
    analytics_db: Option<&'a RegisteredGlobalDb>,
}

async fn run_skill_writer_for_store(
    runtime: SkillWriterStoreRuntime<'_>,
    retrieval: &dyn AutomationSessionRetrieval,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    options: SkillWriterAutomationOptions,
) -> Result<SkillWriterAutomationRun> {
    let SkillWriterStoreRuntime {
        dashboard_root,
        sessions_db,
        analytics_project_root,
        analytics_db,
    } = runtime;
    let mut run = AgentTaskRunContext::new(
        dashboard_root,
        sessions_db,
        options.run_id.clone(),
        "skill_writer",
        options.trigger,
        config,
        AgentTaskKind::SkillWriter,
    );
    if let Some(reason @ ("automation_disabled" | "skill_writer_disabled")) =
        task_skip_reason(config, AgentTaskKind::SkillWriter)
    {
        return Ok(rejected_skill_writer_run(&run, config, reason, None));
    }
    let evidence_bundle = match build_skill_writer_evidence(
        retrieval,
        analytics_project_root,
        analytics_db.map(|database| database as &dyn AutomationSessionStore),
        options,
    )
    .await?
    {
        SkillWriterEvidenceOutcome::Ready(bundle) => bundle,
        SkillWriterEvidenceOutcome::Skipped {
            reason,
            evidence_hash,
        } => {
            return Ok(rejected_skill_writer_run(
                &run,
                config,
                reason,
                evidence_hash,
            ));
        }
    };
    let SkillWriterEvidenceBundle {
        profile_root,
        evidence,
        evidence_hash,
    } = evidence_bundle;
    let _run_lock = match run.gate().await? {
        SchedulerGate::Proceed(lock) => lock,
        SchedulerGate::Skip(reason) => {
            return skipped_skill_writer_run(&run, reason, evidence_hash.clone()).await;
        }
    };

    // Refresh adoption outcomes of previously approved skills so this run's
    // feedback artifact reports real post-approval quality. Best effort: a
    // stale snapshot must not block skill writing.
    if let Err(err) = super::outcomes::refresh_skill_outcomes(
        &profile_root,
        &run.dashboard_root,
        current_timestamp(),
    )
    .await
    {
        tracing::warn!(error = %err, "failed to refresh skill outcomes");
    }

    let activation_policy = skill_writer_activation_policy(config);
    let request = AgentTaskRequest::new(
        run.run_id.clone(),
        AgentTaskKind::SkillWriter,
        build_skill_writer_prompt(&evidence),
        evidence_hash.clone(),
        json!({
            "skill_writer_evidence": evidence,
            "apply": false,
            "activation_policy": activation_policy,
        }),
    );
    let input_hash = Some(request.input_hash.clone());
    let finalizer = run.finalizer(input_hash.clone());
    let (response, retry_report) = match finalizer
        .run_backend_or_fallback(backend, &request, evidence_hash.clone())
        .await?
    {
        BackendTaskRun::Response {
            response,
            retry_report,
        } => (response, retry_report),
        BackendTaskRun::Fallback(record) => {
            let record = *record;
            return Ok(SkillWriterAutomationRun {
                run_id: record.run_id.clone(),
                report: failed_backend_fallback_report(&record),
                ledger_record: record,
                backend_response: None,
            });
        }
    };
    let (proposed_ops, proposals) = finalizer
        .response_output_array(
            &response,
            evidence_hash.clone(),
            &retry_report,
            "skills",
            "skill writer output must include a skills array",
        )
        .await?;
    let (report, record) = match finalize_skill_writer_success(
        &finalizer,
        &profile_root,
        activation_policy,
        ProposedAgentOutput {
            response: &response,
            retry_report: &retry_report,
            evidence: &evidence,
            evidence_hash: evidence_hash.clone(),
            proposed_ops: &proposed_ops,
            proposals: &proposals,
        },
    )
    .await
    {
        Ok(result) => result,
        Err(err) => {
            finalizer
                .append_failed_record(
                    response.model.clone(),
                    evidence_hash,
                    Some(proposed_ops),
                    err.to_string(),
                    &retry_report,
                )
                .await?;
            return Err(err);
        }
    };
    let record = finalizer
        .append_success_record(&request, &response, &retry_report, record)
        .await?;

    Ok(SkillWriterAutomationRun {
        run_id: run.run_id,
        report,
        ledger_record: record,
        backend_response: Some(response),
    })
}

/// Validates and stages the `skills` half of a skill-writer (or combined)
/// run, returning the report plus the not-yet-appended success ledger record.
async fn finalize_skill_writer_success(
    finalizer: &AgentRunFinalizer<'_>,
    profile_root: &std::path::Path,
    activation_policy: &'static str,
    output: ProposedAgentOutput<'_>,
) -> Result<(Value, AutomationRunLedgerRecord)> {
    let ProposedAgentOutput {
        response,
        retry_report: _,
        evidence,
        evidence_hash,
        proposed_ops,
        proposals,
    } = output;
    let config = finalizer.config();
    let run_id = finalizer.run_id();
    let proposal_outcome = validate_and_apply_skill_proposals(
        profile_root,
        run_id,
        proposals,
        config.auto_enable_skills,
    )
    .await?;
    let accepted_count = proposal_outcome.created.len()
        + proposal_outcome.updated.len()
        + proposal_outcome.consolidations.len();
    let rejected_count = proposal_outcome.rejected.len();
    let report = json!({
        "status": if config.auto_enable_skills { "auto_enabled" } else { "needs_approval" },
        "dry_run": !config.auto_enable_skills,
        "task": "skill_writer",
        "evidence_hash": evidence_hash,
        "activation_policy": activation_policy,
        "created_skills": proposal_outcome.created,
        "updated_skills": proposal_outcome.updated,
        "staged_consolidations": proposal_outcome.consolidations,
        "rejected_skills": proposal_outcome.rejected,
        "skill_improvement_recommendations": evidence
            .get("skill_improvement_recommendations")
            .cloned()
            .unwrap_or_else(|| json!([])),
    });
    let mut record = finalizer.success_record(
        response,
        report
            .get("evidence_hash")
            .and_then(Value::as_str)
            .map(str::to_string),
        Some(json!({
            "skills": proposed_ops.get("skills").cloned().unwrap_or_else(|| json!([])),
            "created_skills": report.get("created_skills").cloned().unwrap_or_else(|| json!([])),
            "updated_skills": report.get("updated_skills").cloned().unwrap_or_else(|| json!([])),
            "staged_consolidations": report.get("staged_consolidations").cloned().unwrap_or_else(|| json!([])),
            "rejected_skills": report.get("rejected_skills").cloned().unwrap_or_else(|| json!([])),
        })),
        accepted_count,
        rejected_count,
    );
    record.applied_ops = Some(json!({
        "created_skills": report.get("created_skills").cloned().unwrap_or_else(|| json!([])),
        "updated_skills": report.get("updated_skills").cloned().unwrap_or_else(|| json!([])),
        "staged_consolidations": report.get("staged_consolidations").cloned().unwrap_or_else(|| json!([])),
    }));
    record.rejected_ops = report.get("rejected_skills").cloned();
    record.validation_report = Some(json!({
        "status": report.get("status").cloned().unwrap_or_else(|| json!("needs_approval")),
        "dry_run": !config.auto_enable_skills,
        "activation_policy": activation_policy,
        "accepted_count": accepted_count,
        "rejected_count": rejected_count,
    }));
    Ok((report, record))
}

fn unpersisted_rejected_parts(
    run: &AgentTaskRunContext<'_>,
    config: &AutomationConfig,
    task: AgentTaskKind,
    reason: &str,
    evidence_hash: Option<String>,
    report_task: &'static str,
) -> (Value, AutomationRunLedgerRecord) {
    let completed_at = current_timestamp().to_string();
    let contract = agent_task_contract(task);
    let report = json!({
        "status": "skipped",
        "reason": reason,
        "dry_run": true,
        "task": report_task,
    });
    let record = AutomationRunLedgerRecord {
        schema_version: 2,
        run_id: run.run_id.clone(),
        trigger: run.trigger,
        task,
        task_key: Some(task_key(task).to_string()),
        backend: config.backend.as_str().to_string(),
        host_mode: Some(config.host_mode.as_str().to_string()),
        prompt_version: Some(prompt_version(task).to_string()),
        response_schema: Some(contract.response_schema),
        strict_json: Some(contract.strict_json),
        model: None,
        status: AutomationRunStatus::Skipped,
        evidence_hash,
        input_hash: None,
        output_hash: None,
        proposed_ops: None,
        applied_ops: None,
        rejected_ops: None,
        validation_report: None,
        reviewed_count: 0,
        accepted_count: 0,
        rejected_count: 0,
        skipped_count: 1,
        error: Some(reason.to_string()),
        error_classification: None,
        error_retryable: None,
        backend_attempt_count: 0,
        backend_attempts: Vec::new(),
        fallback_status: Some(reason.to_string()),
        report_ref: Some(json!({
            "dashboard_runs": "/api/plugins/holographic/curation/runs",
            "run_id": run.run_id,
        })),
        artifacts: Vec::new(),
        started_at: run.started_at().to_string(),
        completed_at,
    };
    (report, record)
}

/// Options for the scheduler-only combined reflector+skill pass. Manual
/// (CLI/dashboard) runs stay per-task; this path exists so one backend call
/// can serve both tasks when they are due in the same scheduler tick.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CombinedReviewAutomationOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default)]
    pub session_reflector: SessionReflectorAutomationOptions,
    #[serde(default)]
    pub skill_writer: SkillWriterAutomationOptions,
    #[serde(default = "scheduler_trigger")]
    pub trigger: AutomationTrigger,
}

fn scheduler_trigger() -> AutomationTrigger {
    AutomationTrigger::Scheduler
}

impl Default for CombinedReviewAutomationOptions {
    fn default() -> Self {
        Self {
            run_id: None,
            session_reflector: SessionReflectorAutomationOptions::default(),
            skill_writer: SkillWriterAutomationOptions::default(),
            trigger: AutomationTrigger::Scheduler,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CombinedReviewAutomationRun {
    pub run_id: String,
    pub session_reflector: SessionReflectorAutomationRun,
    pub skill_writer: SkillWriterAutomationRun,
}

struct CombinedReviewEvidence<'a> {
    reflector: &'a SessionReflectorEvidenceBundle,
    skill: &'a SkillWriterEvidenceBundle,
}

/// Outcome of attempting the combined dispatch. `NotCombined` means the
/// caller should fall back to the normal sequential per-task runs; nothing
/// was recorded and no locks are held.
#[derive(Debug)]
pub enum CombinedReviewDispatch {
    Ran(Box<CombinedReviewAutomationRun>),
    RecordedFailure {
        run: Box<CombinedReviewAutomationRun>,
        error: TraceDecayError,
    },
    NotCombined {
        reason: &'static str,
    },
}

/// Runs the session reflector and the skill writer as one combined backend
/// call when both tasks are due in the same scheduler tick.
///
/// Both per-task scheduler gates must proceed (their locks are held for the
/// whole combined run) and both evidence bundles must be available;
/// otherwise the dispatch reports `NotCombined` and the caller runs the
/// tasks sequentially as before. On a combined run, two ledger records are
/// appended — one per task, so per-task last-run bookkeeping and the
/// dashboard scheduler status stay coherent — sharing the combined request's
/// `input_hash` and a `combined_run_id` correlation in `report_ref`, with
/// `prompt_version` set to the combined contract's version.
pub async fn run_combined_review_with_backend(
    cg: &TraceDecay,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    options: CombinedReviewAutomationOptions,
) -> Result<CombinedReviewDispatch> {
    let retrieval = production_project_automation_retrieval(cg).await;
    run_combined_review_for_retrieval(cg, config, backend, retrieval.as_ref(), options).await
}

pub async fn run_combined_review_with_backend_and_retrieval(
    cg: &TraceDecay,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    retrieval: &dyn AutomationSessionRetrieval,
    options: CombinedReviewAutomationOptions,
) -> Result<CombinedReviewDispatch> {
    run_combined_review_for_retrieval(cg, config, backend, retrieval, options).await
}

/// Gate one combined-review sub-task and hand its scheduler lock back to the
/// caller, so the lock lives for the caller's whole run rather than this
/// helper's frame.
///
/// `Ok(Err(dispatch))` is the not-due answer the caller returns verbatim.
async fn acquire_combined_task_lock(
    config: &AutomationConfig,
    dashboard_root: &Path,
    sessions_db: &RegisteredGlobalDb,
    task: AgentTaskKind,
    trigger: AutomationTrigger,
    not_due_reason: &'static str,
) -> Result<std::result::Result<Option<AutomationTaskLock>, CombinedReviewDispatch>> {
    let (gate, _) = task_run_gate(config, dashboard_root, sessions_db, task, trigger).await?;
    Ok(match gate {
        SchedulerGate::Proceed(lock) => Ok(lock),
        SchedulerGate::Skip(_) => Err(CombinedReviewDispatch::NotCombined {
            reason: not_due_reason,
        }),
    })
}

async fn run_combined_review_for_retrieval(
    cg: &TraceDecay,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    retrieval: &dyn AutomationSessionRetrieval,
    options: CombinedReviewAutomationOptions,
) -> Result<CombinedReviewDispatch> {
    if !config.combine_due_tasks {
        return Ok(CombinedReviewDispatch::NotCombined {
            reason: "combined_mode_disabled",
        });
    }
    let dashboard_root = cg.store_layout().dashboard_root.clone();
    let sessions_db = project_automation_sessions(cg).await?;
    let memory =
        MemoryApplication::new(cg.project_memory_owner()?, DatabaseFactStore::new(cg.db()))
            .map_err(|error| TraceDecayError::Config {
                message: format!("could not initialize combined review memory authority: {error}"),
            })?;
    let started_at = current_timestamp().to_string();

    let reflector_bundle =
        match build_session_reflector_evidence(retrieval, &options.session_reflector).await? {
            SessionReflectorEvidenceOutcome::Ready(bundle) => bundle,
            SessionReflectorEvidenceOutcome::Skipped { .. } => {
                return Ok(CombinedReviewDispatch::NotCombined {
                    reason: "session_reflector_evidence_unavailable",
                });
            }
        };
    let skill_bundle = match build_skill_writer_evidence(
        retrieval,
        Some(cg.project_root()),
        Some(cg.profile_database().as_ref()),
        options.skill_writer,
    )
    .await?
    {
        SkillWriterEvidenceOutcome::Ready(bundle) => bundle,
        SkillWriterEvidenceOutcome::Skipped { .. } => {
            return Ok(CombinedReviewDispatch::NotCombined {
                reason: "skill_writer_evidence_unavailable",
            });
        }
    };
    let evidence_bundles = CombinedReviewEvidence {
        reflector: &reflector_bundle,
        skill: &skill_bundle,
    };

    let _reflector_lock = match acquire_combined_task_lock(
        config,
        &dashboard_root,
        sessions_db.as_ref(),
        AgentTaskKind::SessionReflector,
        options.trigger,
        "session_reflector_not_due",
    )
    .await?
    {
        Ok(lock) => lock,
        Err(dispatch) => return Ok(dispatch),
    };
    let _skill_lock = match acquire_combined_task_lock(
        config,
        &dashboard_root,
        sessions_db.as_ref(),
        AgentTaskKind::SkillWriter,
        options.trigger,
        "skill_writer_not_due",
    )
    .await?
    {
        Ok(lock) => lock,
        Err(dispatch) => return Ok(dispatch),
    };
    if let Err(err) =
        super::outcomes::refresh_fact_outcomes(&dashboard_root, &memory, current_timestamp()).await
    {
        tracing::warn!(error = %err, "failed to refresh fact outcomes");
    }

    let run_id = options
        .run_id
        .unwrap_or_else(|| generated_run_id("combined_review"));
    let reflector_run_id = format!("{run_id}_facts");
    let skill_run_id = format!("{run_id}_skills");
    let activation_policy = skill_writer_activation_policy(config);
    let combined_evidence_hash = Some(canonical_evidence_hash(&json!({
        "session_reflection_evidence": reflector_bundle.evidence,
        "skill_writer_evidence": skill_bundle.evidence,
    })));
    let request = AgentTaskRequest::new(
        run_id.clone(),
        AgentTaskKind::CombinedReview,
        build_combined_review_prompt(&reflector_bundle.evidence, &skill_bundle.evidence),
        combined_evidence_hash,
        json!({
            "session_reflection_evidence": reflector_bundle.evidence,
            "skill_writer_evidence": skill_bundle.evidence,
            "apply": false,
            "activation_policy": activation_policy,
        }),
    );
    let input_hash = Some(request.input_hash.clone());
    let reflector_finalizer = AgentRunFinalizer::new(
        &dashboard_root,
        &reflector_run_id,
        options.trigger,
        config,
        AgentTaskKind::SessionReflector,
        &started_at,
        input_hash.clone(),
    )
    .for_combined_run(run_id.clone());
    let skill_finalizer = AgentRunFinalizer::new(
        &dashboard_root,
        &skill_run_id,
        options.trigger,
        config,
        AgentTaskKind::SkillWriter,
        &started_at,
        input_hash,
    )
    .for_combined_run(run_id.clone());

    let retry_policy = BackendRetryPolicy::from_timeout_secs(config.timeout_secs);
    let mut retry_report = AgentTaskRetryReport::default();
    let response =
        match run_agent_task_with_retry_report(backend, &request, &retry_policy, &mut retry_report)
            .await
        {
            Ok(response) => response,
            Err(err) => {
                let reflector_record = reflector_finalizer
                    .append_backend_fallback_record(
                        reflector_bundle.evidence_hash.clone(),
                        err.to_string(),
                        &retry_report,
                    )
                    .await?;
                let skill_record = skill_finalizer
                    .append_backend_fallback_record(
                        skill_bundle.evidence_hash.clone(),
                        err.to_string(),
                        &retry_report,
                    )
                    .await?;
                return Ok(CombinedReviewDispatch::Ran(Box::new(
                    CombinedReviewAutomationRun {
                        run_id,
                        session_reflector: SessionReflectorAutomationRun {
                            run_id: reflector_record.run_id.clone(),
                            report: failed_backend_fallback_report(&reflector_record),
                            ledger_record: reflector_record,
                            backend_response: None,
                        },
                        skill_writer: SkillWriterAutomationRun {
                            run_id: skill_record.run_id.clone(),
                            report: failed_backend_fallback_report(&skill_record),
                            ledger_record: skill_record,
                            backend_response: None,
                        },
                    },
                )));
            }
        };

    let output = match response
        .output_json
        .clone()
        .map_or_else(|| extract_json_object_prefix(&response.output_text), Ok)
    {
        Ok(output) => output,
        Err(err) => {
            let (reflector_record, skill_record) = append_combined_failed_records(
                &reflector_finalizer,
                &skill_finalizer,
                &response,
                &evidence_bundles,
                None,
                &err,
                &retry_report,
            )
            .await?;
            return Ok(CombinedReviewDispatch::RecordedFailure {
                run: combined_failed_run(run_id, reflector_record, skill_record, &response),
                error: err,
            });
        }
    };
    let facts = output.get("facts").and_then(Value::as_array).cloned();
    let skills = output.get("skills").and_then(Value::as_array).cloned();
    let (Some(facts), Some(skills)) = (facts, skills) else {
        let err = TraceDecayError::Config {
            message: "combined review output must include facts and skills arrays".to_string(),
        };
        let (reflector_record, skill_record) = append_combined_failed_records(
            &reflector_finalizer,
            &skill_finalizer,
            &response,
            &evidence_bundles,
            Some(&output),
            &err,
            &retry_report,
        )
        .await?;
        return Ok(CombinedReviewDispatch::RecordedFailure {
            run: combined_failed_run(run_id, reflector_record, skill_record, &response),
            error: err,
        });
    };
    if !facts.is_empty() || !skills.is_empty() {
        let err = TraceDecayError::Config {
            message: "combined review proposals require an atomic apply authority".to_string(),
        };
        let (reflector_record, skill_record) = append_combined_failed_records(
            &reflector_finalizer,
            &skill_finalizer,
            &response,
            &evidence_bundles,
            Some(&output),
            &err,
            &retry_report,
        )
        .await?;
        return Ok(CombinedReviewDispatch::RecordedFailure {
            run: combined_failed_run(run_id, reflector_record, skill_record, &response),
            error: err,
        });
    }

    let (reflector_report, reflector_record) = match finalize_session_reflector_success(
        &memory,
        Some(cg.store_layout().project_root.as_path()),
        &reflector_finalizer,
        ProposedAgentOutput {
            response: &response,
            retry_report: &retry_report,
            evidence: &reflector_bundle.evidence,
            evidence_hash: reflector_bundle.evidence_hash.clone(),
            proposed_ops: &output,
            proposals: &facts,
        },
    )
    .await
    {
        Ok(result) => result,
        Err(err) => {
            let (reflector_record, skill_record) = append_combined_failed_records(
                &reflector_finalizer,
                &skill_finalizer,
                &response,
                &evidence_bundles,
                Some(&output),
                &err,
                &retry_report,
            )
            .await?;
            return Ok(CombinedReviewDispatch::RecordedFailure {
                run: combined_failed_run(run_id, reflector_record, skill_record, &response),
                error: err,
            });
        }
    };

    let (skill_report, skill_record) = match finalize_skill_writer_success(
        &skill_finalizer,
        &skill_bundle.profile_root,
        activation_policy,
        ProposedAgentOutput {
            response: &response,
            retry_report: &retry_report,
            evidence: &skill_bundle.evidence,
            evidence_hash: skill_bundle.evidence_hash.clone(),
            proposed_ops: &output,
            proposals: &skills,
        },
    )
    .await
    {
        Ok(result) => result,
        Err(err) => {
            let (reflector_record, skill_record) = append_combined_failed_records(
                &reflector_finalizer,
                &skill_finalizer,
                &response,
                &evidence_bundles,
                Some(&output),
                &err,
                &retry_report,
            )
            .await?;
            return Ok(CombinedReviewDispatch::RecordedFailure {
                run: combined_failed_run(run_id, reflector_record, skill_record, &response),
                error: err,
            });
        }
    };
    // Finalize both halves before either ledger append. Non-empty proposal
    // sets already failed closed above; empty arrays may append both successes.
    let reflector_record = reflector_finalizer
        .append_success_record(&request, &response, &retry_report, reflector_record)
        .await?;
    let skill_record = skill_finalizer
        .append_success_record(&request, &response, &retry_report, skill_record)
        .await?;

    Ok(CombinedReviewDispatch::Ran(Box::new(
        CombinedReviewAutomationRun {
            run_id,
            session_reflector: SessionReflectorAutomationRun {
                run_id: reflector_run_id,
                report: reflector_report,
                ledger_record: reflector_record,
                backend_response: Some(response.clone()),
            },
            skill_writer: SkillWriterAutomationRun {
                run_id: skill_run_id,
                report: skill_report,
                ledger_record: skill_record,
                backend_response: Some(response),
            },
        },
    )))
}

fn combined_failed_run(
    run_id: String,
    reflector_record: AutomationRunLedgerRecord,
    skill_record: AutomationRunLedgerRecord,
    response: &AgentTaskResponse,
) -> Box<CombinedReviewAutomationRun> {
    Box::new(CombinedReviewAutomationRun {
        run_id,
        session_reflector: SessionReflectorAutomationRun {
            run_id: reflector_record.run_id.clone(),
            report: failed_backend_fallback_report(&reflector_record),
            ledger_record: reflector_record,
            backend_response: Some(response.clone()),
        },
        skill_writer: SkillWriterAutomationRun {
            run_id: skill_record.run_id.clone(),
            report: failed_backend_fallback_report(&skill_record),
            ledger_record: skill_record,
            backend_response: Some(response.clone()),
        },
    })
}

/// Records the same failure for both halves of a combined run so each task's
/// cooldown/retry bookkeeping sees it.
async fn append_combined_failed_records(
    reflector_finalizer: &AgentRunFinalizer<'_>,
    skill_finalizer: &AgentRunFinalizer<'_>,
    response: &AgentTaskResponse,
    evidence: &CombinedReviewEvidence<'_>,
    proposed_ops: Option<&Value>,
    err: &TraceDecayError,
    retry_report: &AgentTaskRetryReport,
) -> Result<(AutomationRunLedgerRecord, AutomationRunLedgerRecord)> {
    let reflector_record = reflector_finalizer
        .append_failed_record(
            response.model.clone(),
            evidence.reflector.evidence_hash.clone(),
            proposed_ops.cloned(),
            err.to_string(),
            retry_report,
        )
        .await?;
    let skill_record = skill_finalizer
        .append_failed_record(
            response.model.clone(),
            evidence.skill.evidence_hash.clone(),
            proposed_ops.cloned(),
            err.to_string(),
            retry_report,
        )
        .await?;
    Ok((reflector_record, skill_record))
}

fn build_combined_review_prompt(reflector_evidence: &Value, skill_evidence: &Value) -> String {
    format!(
        "This is a combined TraceDecay self-improvement review covering both session reflection and skill writing in one pass. Return only one JSON object containing both a facts array and a skills array; use an empty array for a part with nothing to propose. Follow each part's instructions exactly.\n\n## Part 1: session reflection\n{}\n\n## Part 2: skill review\n{}",
        build_session_reflector_prompt(reflector_evidence),
        build_skill_writer_prompt(skill_evidence)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};
    use std::path::PathBuf;

    struct RecordingDenyAutomationAuthorizer {
        requests: Arc<Mutex<Vec<SessionScopeAuthorizationRequest>>>,
    }

    impl SessionScopeAuthorizer for RecordingDenyAutomationAuthorizer {
        fn authorize(
            &self,
            _context: &RequestContext,
            _binding: &SessionRequestBinding,
            request: &SessionScopeAuthorizationRequest,
        ) -> std::result::Result<SessionAuthorizationGrant, SessionAuthorizationError> {
            self.requests.lock().unwrap().push(request.clone());
            Err(SessionAuthorizationError::Denied)
        }
    }

    struct NeverAutomationExecution;

    impl SessionTemporalExecutionPort for NeverAutomationExecution {
        fn execute<'a, E>(
            &'a self,
            _request: crate::application::session::AuthorizedTemporalExecutionRequest,
            _estimator: &'a E,
        ) -> crate::application::session::TemporalExecutionFuture<'a>
        where
            E: VersionedTokenEstimator + Sync + 'a,
        {
            Box::pin(async { panic!("denied retrieval must not reach temporal execution") })
        }
    }

    fn authorized_retrieval_context() -> (RequestContext, SessionRequestBinding) {
        let actor = ActorId::new("automation.session-evidence").unwrap();
        let request_id = RequestId::new("request.automation.session-evidence.test").unwrap();
        let identity = ResolvedSessionIdentity::for_project(
            ProfileId::new("profile.test").unwrap(),
            ProjectId::new("project.test").unwrap(),
            SessionStoreId::new("store.project.test").unwrap(),
            SessionRootId::new("root.project.test").unwrap(),
            ResolvedGitRoute::new(
                RepositoryId::new("repository.test").unwrap(),
                WorktreeId::new("worktree.test").unwrap(),
                BranchId::new("main").unwrap(),
            ),
        );
        let scope = identity.application_scope().unwrap();
        let capability = CapabilityDigest::new([0x11; 32]);
        let policy = PolicyDigest::new([0x22; 32]);
        let configuration = ConfigurationDigest::new([0x33; 32]);
        let cancellation = CancellationToken::for_application_request(request_id.as_str());
        let budgets = RequestBudgets::new(128, AUTOMATION_SESSION_MAX_BYTES, 10_000).unwrap();
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.automation.session-evidence.test").unwrap(),
            1,
            session_application_grant_digest(
                capability,
                policy,
                configuration,
                &cancellation,
                budgets,
            )
            .unwrap(),
            actor.clone(),
            UtcMicros(1),
            UtcMicros(i64::MAX - 1),
            scope.clone(),
            BTreeSet::from([CapabilityId::new("capability.session.temporal-retrieval").unwrap()]),
            BTreeSet::from([UseCaseId::new("use-case.automation.session-evidence").unwrap()]),
            DisclosureClass::Evidence,
        )
        .unwrap();
        let context = RequestContext::new(
            actor,
            scope,
            grant,
            request_id.clone(),
            Deadline::new(UtcMicros(i64::MAX - 1)).unwrap(),
            CancellationContext::active(cancellation.application_token_id().unwrap()).unwrap(),
        )
        .unwrap();
        let binding = SessionRequestBinding::new(
            identity,
            capability,
            policy,
            configuration,
            cancellation,
            budgets,
        );
        (context, binding)
    }

    #[tokio::test]
    async fn real_authorized_service_path_denies_before_execution() {
        let authorization_requests = Arc::new(Mutex::new(Vec::new()));
        let service = SessionRetrievalService::new(
            RecordingDenyAutomationAuthorizer {
                requests: Arc::clone(&authorization_requests),
            },
            NeverAutomationExecution,
            AutomationWordEstimator,
            SessionRetrievalConfiguration::new(1, 1).unwrap(),
        );
        let (context, binding) = authorized_retrieval_context();
        let adapter = AuthorizedAutomationSessionRetrieval::new(
            &service,
            &context,
            &binding,
            SessionId::new("session.authorized.test").unwrap(),
        );
        let outcome = retrieve_automation_session_evidence(
            &adapter,
            "authorized test",
            LcmScope::All,
            AutomationEvidenceFilters {
                provider: "cursor",
                session_id: None,
                include_summaries: true,
                evidence_limit: 5,
                include_recent_sessions: false,
                recent_sessions_limit: 1,
                role: None,
                start_time: None,
                end_time: None,
                sort: LcmGrepSort::Relevance,
            },
        )
        .await
        .unwrap();

        assert!(matches!(
            outcome,
            AutomationTemporalRetrieval::Rejected("session_evidence_denied")
        ));
        let requests = authorization_requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].temporal_mode(),
            tracedecay_domain::TemporalModeV1::Forensic
        );
        assert_eq!(requests[0].grain(), RetrievalGrainV1::LogicalMessage);
        assert_eq!(requests[0].access(), SessionAccess::Hydrate);
    }

    #[test]
    fn temporal_automation_evidence_fails_closed_for_non_complete_outcomes() {
        for (outcome, expected_reason) in [
            (
                SessionRetrievalOutcome::<TemporalKernelResult>::Stale {
                    freshness: crate::application::session::SessionDataFreshness::Stored {
                        generation_lag: 1,
                    },
                },
                "session_evidence_stale",
            ),
            (
                SessionRetrievalOutcome::Partial {
                    items: Vec::new(),
                    freshness: crate::application::session::SessionDataFreshness::Fresh,
                    omitted: 1,
                },
                "session_evidence_partial",
            ),
            (SessionRetrievalOutcome::Denied, "session_evidence_denied"),
            (
                SessionRetrievalOutcome::BudgetExhausted,
                "session_evidence_budget_exhausted",
            ),
            (
                SessionRetrievalOutcome::Cancelled,
                "session_evidence_cancelled",
            ),
            (
                SessionRetrievalOutcome::CompleteZero {
                    freshness: crate::application::session::SessionDataFreshness::Stored {
                        generation_lag: 2,
                    },
                },
                "session_evidence_stale",
            ),
        ] {
            assert!(matches!(
                accept_automation_temporal_outcome(outcome),
                AutomationTemporalRetrieval::Rejected(reason) if reason == expected_reason
            ));
        }
        assert!(matches!(
            accept_automation_temporal_outcome(
                SessionRetrievalOutcome::<TemporalKernelResult>::CompleteZero {
                    freshness: crate::application::session::SessionDataFreshness::Fresh,
                }
            ),
            AutomationTemporalRetrieval::CompleteZero
        ));
    }

    #[test]
    fn complete_temporal_outcome_independently_requires_fresh_coverage() {
        let stale = SessionRetrievalOutcome::<TemporalKernelResult>::Complete {
            items: Vec::new(),
            freshness: crate::application::session::SessionDataFreshness::Stored {
                generation_lag: 1,
            },
        };
        // Empty Complete is invalid at the type layer, but stale freshness must
        // still fail closed before any serialization or write path.
        assert!(matches!(
            accept_automation_temporal_outcome(stale),
            AutomationTemporalRetrieval::Rejected("session_evidence_stale")
        ));
    }

    struct RecordingRejectedAutomationRetrieval {
        anchor_session_id: SessionId,
        queries: Mutex<Vec<SessionTemporalQuery>>,
    }

    impl AutomationSessionRetrieval for RecordingRejectedAutomationRetrieval {
        fn anchor_session_id(&self) -> &SessionId {
            &self.anchor_session_id
        }

        fn retrieve(&self, query: SessionTemporalQuery) -> AutomationSessionRetrievalFuture<'_> {
            self.queries.lock().unwrap().push(query);
            Box::pin(async { AutomationTemporalRetrieval::Rejected("session_evidence_denied") })
        }
    }

    #[tokio::test]
    async fn automation_retrieval_requests_fresh_forensic_evidence_and_preserves_rejection() {
        let retrieval = RecordingRejectedAutomationRetrieval {
            anchor_session_id: SessionId::new("session.automation.recording").unwrap(),
            queries: Mutex::new(Vec::new()),
        };
        let outcome = retrieve_automation_session_evidence(
            &retrieval,
            "record the canonical request",
            LcmScope::All,
            AutomationEvidenceFilters {
                provider: "cursor",
                session_id: None,
                include_summaries: true,
                evidence_limit: 5,
                include_recent_sessions: true,
                recent_sessions_limit: 3,
                role: None,
                start_time: None,
                end_time: None,
                sort: LcmGrepSort::Relevance,
            },
        )
        .await
        .unwrap();

        assert!(matches!(
            outcome,
            AutomationTemporalRetrieval::Rejected("session_evidence_denied")
        ));
        let queries = retrieval.queries.lock().unwrap();
        assert_eq!(queries.len(), 1);
        assert_eq!(
            queries[0].temporal_mode(),
            tracedecay_domain::TemporalModeV1::Forensic
        );
        assert_eq!(
            queries[0].freshness_policy(),
            SessionFreshnessPolicy::RequireFresh
        );
        assert_eq!(queries[0].grain(), RetrievalGrainV1::LogicalMessage);
    }

    #[test]
    fn builders_reject_hidden_unknown_and_redacted_complete_evidence() {
        for coverage in [
            TemporalCoverageCountsV1 {
                visible: 1,
                hidden: 1,
                unknown: 0,
                redacted: 0,
            },
            TemporalCoverageCountsV1 {
                visible: 1,
                hidden: 0,
                unknown: 1,
                redacted: 0,
            },
            TemporalCoverageCountsV1 {
                visible: 1,
                hidden: 0,
                unknown: 0,
                redacted: 1,
            },
        ] {
            let evidence = AutomationTemporalEvidence {
                items: vec![AutomationTemporalEvidenceItem {
                    anchor_id: "coverage-anchor".to_string(),
                    stable_id: "coverage-stable".to_string(),
                    provider: "cursor".to_string(),
                    session_id: "coverage-session".to_string(),
                    message_id: Some("coverage-message".to_string()),
                    source_id: Some("coverage-source".to_string()),
                    store_id: Some(1),
                    role: Some("user".to_string()),
                    ordinal: Some(1),
                    session_total_messages: Some(1),
                    knowledge_at_micros: 1,
                    normalized_score_micros: 1,
                    snippet: "coverage".to_string(),
                }],
                coverage,
            };
            assert_eq!(
                validate_complete_evidence(&evidence),
                Err("session_evidence_partial")
            );
        }
    }

    #[test]
    fn temporal_automation_serializer_preserves_citations_bounds_and_hashes() {
        let oversized = "x".repeat(SESSION_REPLAY_SNIPPET_CHARS + 25);
        let filters = AutomationEvidenceFilters {
            provider: "cursor",
            session_id: None,
            include_summaries: true,
            evidence_limit: 5,
            include_recent_sessions: true,
            recent_sessions_limit: 3,
            role: None,
            start_time: None,
            end_time: None,
            sort: LcmGrepSort::Recency,
        };
        let serialized = serialize_automation_temporal_evidence(
            AutomationTemporalEvidence {
                items: vec![AutomationTemporalEvidenceItem {
                    anchor_id: "anchor-1".to_string(),
                    stable_id: "stable-1".to_string(),
                    provider: "cursor".to_string(),
                    session_id: "session-1".to_string(),
                    message_id: Some("message-1".to_string()),
                    source_id: Some("occurrence-1".to_string()),
                    store_id: None,
                    role: Some("user".to_string()),
                    ordinal: Some(1),
                    session_total_messages: Some(1),
                    knowledge_at_micros: 1_715_000_001_000_000,
                    normalized_score_micros: 1_000_000,
                    snippet: oversized,
                }],
                coverage: TemporalCoverageCountsV1 {
                    visible: 1,
                    hidden: 0,
                    unknown: 0,
                    redacted: 0,
                },
            },
            filters,
        );

        assert_eq!(serialized.hits[0].kind, "raw_message");
        assert_eq!(serialized.hits[0].session_id, "session-1");
        assert_eq!(serialized.hits[0].message_id.as_deref(), Some("message-1"));
        assert_eq!(serialized.hits[0].node_id.as_deref(), Some("occurrence-1"));
        assert_eq!(serialized.hits[0].anchor_id, "anchor-1");
        assert_eq!(serialized.hits[0].stable_id, "stable-1");
        assert_eq!(
            serialized.hits[0].snippet.chars().count(),
            SESSION_REPLAY_SNIPPET_CHARS
        );
        let replay = serialized.recent_session_slices.unwrap();
        assert_eq!(
            replay["sessions"][0]["head"][0]["message_id"],
            json!("message-1")
        );
        assert_eq!(replay["sessions"][0]["provider"], json!("cursor"));
        assert_eq!(replay["sessions"][0]["total_messages"], json!(1));
        assert_eq!(replay["sessions"][0]["head"][0]["ordinal"], json!(1));
        assert_eq!(
            replay["sessions"][0]["head"][0]["anchor_id"],
            json!("anchor-1")
        );
        assert_eq!(
            replay["bounds"]["snippet_chars"],
            json!(SESSION_REPLAY_SNIPPET_CHARS)
        );
        let mut evidence = json!({
            "hits": serialized.hits,
            "recent_session_slices": replay,
            "temporal_coverage": serialized.coverage,
        });
        let first_hash = canonical_evidence_hash(&evidence);
        evidence["hits"][0]["message_id"] = json!("message-2");
        assert!(first_hash.starts_with("sha256:"));
        assert_ne!(first_hash, canonical_evidence_hash(&evidence));
    }

    #[test]
    fn canonical_evidence_is_permutation_stable_and_request_bound() {
        let item = |provider: &str, anchor: &str, ordinal: i64, score: u64| {
            AutomationTemporalEvidenceItem {
                anchor_id: anchor.to_string(),
                stable_id: format!("stable-{anchor}"),
                provider: provider.to_string(),
                session_id: "session-canonical".to_string(),
                message_id: Some(format!("message-{ordinal}")),
                source_id: Some(format!("occurrence-{ordinal}")),
                store_id: Some(ordinal),
                role: Some("user".to_string()),
                ordinal: Some(ordinal),
                session_total_messages: Some(2),
                knowledge_at_micros: 1_715_000_000_000_000 + ordinal,
                normalized_score_micros: score,
                snippet: format!("payload-{ordinal}"),
            }
        };
        let filters = AutomationEvidenceFilters {
            provider: "all",
            session_id: None,
            include_summaries: true,
            evidence_limit: 1,
            include_recent_sessions: false,
            recent_sessions_limit: 3,
            role: None,
            start_time: None,
            end_time: None,
            sort: LcmGrepSort::Relevance,
        };
        let serialize = |items| {
            serialize_automation_temporal_evidence(
                AutomationTemporalEvidence {
                    items,
                    coverage: TemporalCoverageCountsV1 {
                        visible: 2,
                        hidden: 0,
                        unknown: 0,
                        redacted: 0,
                    },
                },
                filters,
            )
        };
        let first = serialize(vec![
            item("cursor", "anchor-a", 1, 10),
            item("codex", "anchor-b", 2, 20),
        ]);
        let second = serialize(vec![
            item("codex", "anchor-b", 2, 20),
            item("cursor", "anchor-a", 1, 10),
        ]);
        let first_value = json!({
            "provider": "all",
            "query": "canonical request",
            "sort": "relevance",
            "hits": first.hits,
            "temporal_coverage": first.coverage,
        });
        let second_value = json!({
            "provider": "all",
            "query": "canonical request",
            "sort": "relevance",
            "hits": second.hits,
            "temporal_coverage": second.coverage,
        });
        let digest = canonical_evidence_hash(&first_value);

        assert_eq!(first_value, second_value);
        assert_eq!(first_value["hits"][0]["provider"], json!("codex"));
        assert_eq!(first_value["hits"][0]["anchor_id"], json!("anchor-b"));
        assert_eq!(first_value["temporal_coverage"]["visible"], json!(1));
        assert_eq!(
            digest,
            "sha256:20c37de4e2fdcca8c190087087c6ad4a0ae1ba2969bcb8cee018c6ec6a6edac3"
        );

        let mut provider_mutation = first_value.clone();
        provider_mutation["provider"] = json!("cursor");
        assert_ne!(digest, canonical_evidence_hash(&provider_mutation));
        let mut query_mutation = first_value;
        query_mutation["query"] = json!("different request");
        assert_ne!(digest, canonical_evidence_hash(&query_mutation));
    }

    #[tokio::test]
    async fn proposal_validation_does_not_wait_for_the_writer_lane() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("memory.db");
        let authority =
            crate::db::DatabaseAuthority::acquire_test(&path, "automation validation writer lane")
                .unwrap();
        let (db, _) = crate::db::Database::publish_test_runtime(
            &path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .unwrap();
        let owner = FactOwnerV1::Profile;
        let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(&db)).unwrap();
        let existing_fact_id = memory
            .add_fact_v1(
                crate::memory::types::AddFactRequest {
                    content: "Committed memory baseline".to_string(),
                    category: crate::memory::types::MemoryCategory::Project,
                    source: None,
                    tags: vec!["automation".to_string()],
                    entities: vec!["TraceDecay".to_string()],
                    trust: Some(0.8),
                    metadata: json!({}),
                },
                crate::application::memory::MemoryOperationContext::generated(
                    &owner,
                    "seed automation validation",
                    None,
                )
                .unwrap(),
            )
            .await
            .unwrap()
            .fact
            .unwrap()
            .fact_id;
        let transaction = db
            .begin_write_transaction("hold automation validation writer")
            .await
            .unwrap();
        transaction
            .execute(
                "UPDATE memory_facts SET updated_at = updated_at WHERE fact_id = ?1",
                [existing_fact_id],
            )
            .await
            .unwrap();
        let proposals = [json!({
            "content": "Validation stays read-only",
            "category": "project",
            "tags": ["automation"],
            "entities": ["TraceDecay"],
            "trust": 0.8,
            "source_span": {"session_id": "session", "message_id": "message"},
            "reason": "bounded test evidence"
        })];
        let evidence = json!({
            "hits": [{
                "kind": "raw_message",
                "session_id": "session",
                "message_id": "message"
            }]
        });

        let validated = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            validate_session_fact_proposals(&memory, &proposals, &evidence),
        )
        .await
        .expect("read-only validation must not wait for writer authority")
        .unwrap();
        assert_eq!(validated.0.len(), 1);
        assert!(validated.1.is_empty());
        assert_eq!(
            memory
                .get_fact_v1(existing_fact_id)
                .await
                .unwrap()
                .unwrap()
                .access_count,
            0
        );
        transaction.rollback().await.unwrap();
    }

    #[tokio::test]
    async fn auto_apply_refreshes_digest_only_for_a_new_authority_promotion() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let profile_root = std::env::var_os(crate::config::USER_DATA_DIR_ENV)
            .map(PathBuf::from)
            .expect("pinned profile root");
        let temp = tempfile::tempdir().unwrap();
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).unwrap();
        let database_path = temp.path().join("memory.db");
        let authority = DatabaseAuthority::acquire_test(
            &database_path,
            "automation proposal digest disposition test",
        )
        .unwrap();
        let (database, _) = Database::publish_test_runtime(
            &database_path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .unwrap();
        let memory =
            MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&database))
                .unwrap();
        let dashboard_root = temp.path().join("dashboard");
        let records = record_session_fact_proposals(
            &memory,
            &dashboard_root,
            "run-digest-disposition",
            None,
            &[json!({
                "add_fact_request": {
                    "content": "Refresh the digest only after a new authority promotion",
                    "category": "project",
                    "source": "automation-test",
                    "tags": ["automation"],
                    "entities": ["TraceDecay"],
                    "trust": 0.9,
                    "metadata": {}
                }
            })],
            &[],
        )
        .await
        .unwrap();

        let (applied, newly_promoted) = auto_apply_session_fact_proposals(
            &memory,
            Some(&project_root),
            &dashboard_root,
            records.clone(),
        )
        .await
        .unwrap();
        assert!(newly_promoted);
        assert_eq!(applied[0].state, FactProposalState::Applied);

        let snapshot = crate::automation::memory_digest::memory_digest_snapshot_path(&profile_root);
        assert!(snapshot.exists(), "new promotion must refresh the digest");
        std::fs::remove_file(&snapshot).unwrap();

        let (replayed, newly_promoted) = auto_apply_session_fact_proposals(
            &memory,
            Some(&project_root),
            &dashboard_root,
            records,
        )
        .await
        .unwrap();
        assert!(
            !newly_promoted,
            "an applied proposal replay is not a promotion"
        );
        assert_eq!(replayed[0].state, FactProposalState::Applied);
        assert!(
            !snapshot.exists(),
            "an idempotent applied replay must not refresh the digest"
        );
    }

    #[tokio::test]
    async fn auto_apply_flushes_a_new_promotion_before_later_conflict_returns() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let profile_root = std::env::var_os(crate::config::USER_DATA_DIR_ENV)
            .map(PathBuf::from)
            .expect("pinned profile root");
        let temp = tempfile::tempdir().unwrap();
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).unwrap();
        let database_path = temp.path().join("memory.db");
        let authority = DatabaseAuthority::acquire_test(
            &database_path,
            "automation proposal partial digest refresh test",
        )
        .unwrap();
        let (database, _) = Database::publish_test_runtime(
            &database_path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .unwrap();
        let owner = FactOwnerV1::Profile;
        let memory =
            MemoryApplication::new(owner.clone(), DatabaseFactStore::new(&database)).unwrap();
        let dashboard_root = temp.path().join("dashboard");
        let records = record_session_fact_proposals(
            &memory,
            &dashboard_root,
            "run-digest-partial",
            None,
            &[
                json!({
                    "add_fact_request": {
                        "content": "A successful promotion must refresh before a later conflict",
                        "category": "project",
                        "source": "automation-test",
                        "tags": ["automation"],
                        "entities": ["TraceDecay"],
                        "trust": 0.9,
                        "metadata": {}
                    }
                }),
                json!({
                    "add_fact_request": {
                        "content": "This proposal is rejected to force the later conflict",
                        "category": "project",
                        "source": "automation-test",
                        "tags": ["automation"],
                        "entities": ["TraceDecay"],
                        "trust": 0.9,
                        "metadata": {}
                    }
                }),
            ],
            &[],
        )
        .await
        .unwrap();
        let rejected_id =
            tracedecay_domain::ProvenanceId::new(records[1].proposal_id.clone()).unwrap();
        let rejected = memory
            .get_compatibility_fact_proposal(rejected_id.clone())
            .await
            .unwrap()
            .unwrap();
        memory
            .reject_compatibility_fact_proposal(
                rejected_id,
                rejected.revision(),
                tracedecay_domain::ActorId::new("test:reviewer".to_string()).unwrap(),
                "fixture conflict".to_string(),
            )
            .await
            .unwrap();

        let error = auto_apply_session_fact_proposals(
            &memory,
            Some(&project_root),
            &dashboard_root,
            records,
        )
        .await
        .expect_err("the rejected second proposal must keep its original error path");
        assert!(error.to_string().contains("not pending approval"));
        assert!(
            crate::automation::memory_digest::memory_digest_snapshot_path(&profile_root).exists(),
            "the first new promotion must still refresh before returning the later conflict"
        );
    }
}
