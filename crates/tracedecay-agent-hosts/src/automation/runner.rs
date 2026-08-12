use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracedecay_domain::configuration::ConfigurationRevisionId;
use tracedecay_domain::{ActorId, FactOwnerV1};

use super::ExternalSkillDeploymentDisposition;
use super::artifacts::sha256_json;
use super::backend::{
    AgentTaskBackend, AgentTaskKind, AgentTaskRequest, AgentTaskResponse, AgentTaskRetryReport,
    BackendRetryPolicy, run_agent_task_with_retry_report,
};
use super::config::AutomationConfig;
use super::lifecycle::{
    AgentRunFinalizer, AutomationCommittedReceipt, AutomationRunControl, AutomationRunError,
    AutomationRunResult, BackendTaskRun, SchedulerGate, failed_backend_fallback_report,
    generated_run_id, task_run_gate,
};
use super::run_ledger::{AutomationRunLedgerRecord, AutomationTrigger};
use super::scheduler::AutomationTaskLock;
use super::skill_writer::{
    activation_policy as skill_writer_activation_policy, validate_and_apply_skill_proposals,
    validate_skill_proposals,
};
use crate::errors::{Result, TraceDecayError};
use crate::ports::project_runtime::ProfileRuntime;
use crate::ports::project_runtime::TraceDecay;
use crate::ports::session_store::AutomationSessionStore;
use crate::store::memory::DatabaseFactStore;
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_policy::CurationApplyAuthorityV1;
use tracedecay_runtime_core::tracedecay::current_timestamp;
use tracedecay_usecases::memory::MemoryApplication;

mod curation;
mod evidence;
mod retrieval;
mod session_reflector;
mod skill_writer;
#[cfg(test)]
mod user_scope_tests;

use curation::{combined_review_output, evaluate_skill_curation};
use evidence::{
    SessionReflectorEvidenceBundle, SessionReflectorEvidenceOutcome, SkillWriterEvidenceBundle,
    SkillWriterEvidenceOutcome, build_session_reflector_evidence, build_skill_writer_evidence,
    canonical_evidence_hash,
};
use retrieval::{production_project_automation_retrieval, production_user_automation_retrieval};
use session_reflector::{
    ProposedAgentOutput, SessionReflectorFinalization, build_session_reflector_prompt,
    finalize_session_reflector_success, validate_session_fact_candidates,
};
use skill_writer::{
    ProposedSkillOutput, SkillWriterFinalization, build_skill_writer_prompt,
    finalize_skill_writer_success, run_user_skill_writer_with_backend_and_retrieval,
};

pub use evidence::{AutomationTemporalEvidence, AutomationTemporalEvidenceItem};
pub use retrieval::registered_project_automation_retrieval;
pub use retrieval::{
    AuthorizedAutomationSessionRetrieval, AutomationSessionRetrieval,
    AutomationSessionRetrievalFuture, AutomationTemporalRetrieval,
};
pub(crate) use session_reflector::run_user_session_reflector_with_backend_and_retrieval;
pub use session_reflector::{
    SessionFactCurationOutcome, SessionFactCurationReceipt, SessionReflectorAutomationOptions,
    SessionReflectorAutomationRun, run_session_reflector_with_backend,
    run_session_reflector_with_backend_and_retrieval,
};
pub use skill_writer::{
    SkillWriterAutomationOptions, SkillWriterAutomationRun, run_skill_writer_with_backend,
    run_skill_writer_with_backend_and_retrieval,
};

pub(crate) use super::memory_curator::run_user_memory_curator_with_backend;
pub use super::memory_curator::{
    CURATION_DEFAULT_FACT_REVIEW_LIMIT, CURATION_DEFAULT_MIN_CONFIDENCE,
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

fn project_curation_authority(
    cg: &TraceDecay,
    actor: &'static str,
    configuration_revision_id: &ConfigurationRevisionId,
) -> Result<CurationApplyAuthorityV1> {
    let FactOwnerV1::Project { project_id } = cg.project_memory_owner()? else {
        return Err(TraceDecayError::Config {
            message: "project curation requires authoritative project scope".to_owned(),
        });
    };
    let actor_id = ActorId::new(actor).map_err(|error| TraceDecayError::Config {
        message: format!("invalid curation actor identity: {error}"),
    })?;
    Ok(CurationApplyAuthorityV1 {
        actor_id,
        project_id: Some(project_id),
        profile_id: cg.profile_id().clone(),
        configuration_revision_id: configuration_revision_id.clone(),
    })
}

fn profile_curation_authority(
    runtime: &dyn ProfileRuntime,
    actor: &'static str,
    configuration_revision_id: &ConfigurationRevisionId,
) -> Result<CurationApplyAuthorityV1> {
    let actor_id = ActorId::new(actor).map_err(|error| TraceDecayError::Config {
        message: format!("invalid curation actor identity: {error}"),
    })?;
    Ok(CurationApplyAuthorityV1 {
        actor_id,
        project_id: None,
        profile_id: runtime.profile_id().clone(),
        configuration_revision_id: configuration_revision_id.clone(),
    })
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
    configuration_revision_id: &ConfigurationRevisionId,
    backend: &dyn AgentTaskBackend,
    options: UserSessionAutomationOptions,
    run_control: &AutomationRunControl,
) -> AutomationRunResult<UserSessionAutomationRun> {
    let retrieval = production_user_automation_retrieval(profile_root).await;
    run_user_session_automation_with_backend_and_retrieval(
        profile_root,
        session_registry,
        config,
        configuration_revision_id,
        backend,
        retrieval.as_ref(),
        options,
        run_control,
    )
    .await
}

pub(crate) async fn run_user_session_automation_with_backend_and_retrieval(
    profile_root: &std::path::Path,
    session_registry: Arc<dyn ProfileRuntime>,
    config: &AutomationConfig,
    configuration_revision_id: &ConfigurationRevisionId,
    backend: &dyn AgentTaskBackend,
    retrieval: &dyn AutomationSessionRetrieval,
    options: UserSessionAutomationOptions,
    run_control: &AutomationRunControl,
) -> AutomationRunResult<UserSessionAutomationRun> {
    let session_reflector = run_user_session_reflector_with_backend_and_retrieval(
        profile_root,
        Arc::clone(&session_registry),
        config,
        run_control,
        configuration_revision_id,
        backend,
        retrieval,
        options.session_reflector,
    )
    .await?;
    let memory_curator = run_user_memory_curator_with_backend(
        profile_root,
        Arc::clone(&session_registry),
        config,
        configuration_revision_id,
        backend,
        options.memory_curator,
        run_control,
    )
    .await?;
    let skill_writer = run_user_skill_writer_with_backend_and_retrieval(
        profile_root,
        session_registry,
        config,
        configuration_revision_id,
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
    MemoryCompletedSkillFailure {
        session_reflector: Box<SessionReflectorAutomationRun>,
        skill_writer_record: Option<AutomationRunLedgerRecord>,
        error: TraceDecayError,
    },
    RecordedFailure {
        run: Box<CombinedReviewAutomationRun>,
        error: TraceDecayError,
    },
    PartialEffect {
        run: Option<Box<CombinedReviewAutomationRun>>,
        completed_session_reflector: Option<Box<SessionReflectorAutomationRun>>,
        run_id: String,
        committed_receipt: AutomationCommittedReceipt,
        detail: &'static str,
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
    configuration_revision_id: &ConfigurationRevisionId,
    backend: &dyn AgentTaskBackend,
    options: CombinedReviewAutomationOptions,
    run_control: &AutomationRunControl,
) -> Result<CombinedReviewDispatch> {
    let retrieval = production_project_automation_retrieval(cg).await;
    run_combined_review_for_retrieval(
        cg,
        config,
        configuration_revision_id,
        backend,
        retrieval.as_ref(),
        options,
        run_control,
    )
    .await
}

pub async fn run_combined_review_with_backend_and_retrieval(
    cg: &TraceDecay,
    config: &AutomationConfig,
    configuration_revision_id: &ConfigurationRevisionId,
    backend: &dyn AgentTaskBackend,
    retrieval: &dyn AutomationSessionRetrieval,
    options: CombinedReviewAutomationOptions,
    run_control: &AutomationRunControl,
) -> Result<CombinedReviewDispatch> {
    run_combined_review_for_retrieval(
        cg,
        config,
        configuration_revision_id,
        backend,
        retrieval,
        options,
        run_control,
    )
    .await
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
    configuration_revision_id: &ConfigurationRevisionId,
    backend: &dyn AgentTaskBackend,
    retrieval: &dyn AutomationSessionRetrieval,
    options: CombinedReviewAutomationOptions,
    run_control: &AutomationRunControl,
) -> Result<CombinedReviewDispatch> {
    let reflector_authority = project_curation_authority(
        cg,
        "automation:session-reflector",
        configuration_revision_id,
    )?;
    let skill_authority =
        project_curation_authority(cg, "automation:skill-writer", configuration_revision_id)?;
    if !config.combine_due_tasks {
        return Ok(CombinedReviewDispatch::NotCombined {
            reason: "combined_mode_disabled",
        });
    }
    let dashboard_root = cg.store_layout().dashboard_root.clone();
    let sessions_db = project_automation_sessions(cg).await?;
    let project_memory_db = cg.open_project_store_db().await?;
    let memory = MemoryApplication::new(
        cg.project_memory_owner()?,
        DatabaseFactStore::new(&project_memory_db),
    )
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
    if let Err(err) = super::outcomes::refresh_fact_outcomes(
        &dashboard_root,
        &memory,
        current_timestamp(),
        run_control.read_control(),
    )
    .await
    {
        tracing::warn!(error = %err, "failed to refresh fact outcomes");
    }

    let run_id = options
        .run_id
        .unwrap_or_else(|| generated_run_id("combined_review"));
    // Canonical automatic-fact receipts bind the admitted outer memory
    // automation identity. Per-task correlation remains in the combined-run
    // ledger annotation rather than inventing a second mutation authority ID.
    let reflector_run_id = run_id.clone();
    let skill_run_id = format!("{run_id}_skills");
    let activation_policy = skill_writer_activation_policy();
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
            "apply": true,
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
    )?
    .for_combined_run(run_id.clone());
    let skill_finalizer = AgentRunFinalizer::new(
        &dashboard_root,
        &skill_run_id,
        options.trigger,
        config,
        AgentTaskKind::SkillWriter,
        &started_at,
        input_hash,
    )?
    .for_combined_run(run_id.clone());

    let retry_policy = BackendRetryPolicy::from_timeout_secs(config.timeout_secs);
    let mut retry_report = AgentTaskRetryReport::default();
    let mut response =
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
                return Ok(CombinedReviewDispatch::RecordedFailure {
                    run: Box::new(CombinedReviewAutomationRun {
                        run_id,
                        session_reflector: SessionReflectorAutomationRun {
                            run_id: reflector_record.run_id.clone(),
                            report: failed_backend_fallback_report(&reflector_record),
                            ledger_record: reflector_record,
                            backend_response: None,
                            committed_receipt: None,
                        },
                        skill_writer: SkillWriterAutomationRun {
                            run_id: skill_record.run_id.clone(),
                            report: failed_backend_fallback_report(&skill_record),
                            ledger_record: skill_record,
                            backend_response: None,
                            committed_receipt: None,
                        },
                    }),
                    error: err,
                });
            }
        };

    let (mut output, mut facts, mut skills) = match combined_review_output(&response) {
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
    let mut validation_repairs = Vec::new();
    loop {
        let (_, fact_errors) = validate_session_fact_candidates(
            &memory,
            run_control,
            &facts,
            &reflector_bundle.evidence,
        )
        .await?;
        let skill_errors =
            validate_skill_proposals(&skill_bundle.profile_root, &skill_run_id, &skills).await?;
        if fact_errors.is_empty() && skill_errors.is_empty() {
            break;
        }
        let attempt = validation_repairs.len() + 1;
        validation_repairs.push(json!({
            "attempt": attempt,
            "fact_errors": fact_errors,
            "skill_errors": skill_errors,
        }));
        if attempt == 2 {
            let err = TraceDecayError::Config {
                message: "combined curation validation repair budget exhausted; output quarantined"
                    .to_string(),
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
        let repair_request = AgentTaskRequest::new(
            run_id.clone(),
            AgentTaskKind::CombinedReview,
            "Repair the previous combined curation JSON. Return only {\"facts\": [...], \"skills\": [...]}. Preserve valid intent, fix every validation error, use only the supplied evidence, and do not add unrelated proposals."
                .to_string(),
            Some(canonical_evidence_hash(&json!({
                "session_reflection_evidence": reflector_bundle.evidence,
                "skill_writer_evidence": skill_bundle.evidence,
            }))),
            json!({
                "previous_output": output.clone(),
                "validation_errors": validation_repairs.last(),
                "session_reflection_evidence": reflector_bundle.evidence.clone(),
                "skill_writer_evidence": skill_bundle.evidence.clone(),
                "activation_policy": activation_policy,
                "apply": true,
            }),
        );
        let mut repair_retry_report = AgentTaskRetryReport::default();
        response = match run_agent_task_with_retry_report(
            backend,
            &repair_request,
            &retry_policy,
            &mut repair_retry_report,
        )
        .await
        {
            Ok(response) => response,
            Err(err) => {
                retry_report.append(repair_retry_report);
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
        retry_report.append(repair_retry_report);
        (output, facts, skills) = match combined_review_output(&response) {
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
    }
    let (reflector_report, reflector_record, reflector_committed_receipt) =
        match finalize_session_reflector_success(
            &memory,
            run_control,
            config,
            &reflector_authority,
            &reflector_finalizer,
            ProposedAgentOutput {
                response: &response,
                retry_report: &retry_report,
                evidence: &reflector_bundle.evidence,
                evidence_hash: reflector_bundle.evidence_hash.clone(),
                proposed_ops: &output,
                proposals: &facts,
            },
            &validation_repairs,
        )
        .await
        {
            Ok(SessionReflectorFinalization::Completed {
                report,
                record,
                committed_receipt,
            }) => (report, record, committed_receipt),
            Ok(SessionReflectorFinalization::FailedRecorded {
                run_id: committed_run_id,
                committed_receipt,
                record,
                detail,
            }) => {
                let skill_record = skill_finalizer
                    .append_failed_record(
                        response.model.clone(),
                        skill_bundle.evidence_hash.clone(),
                        Some(combined_skill_failure_projection(&output)),
                        detail.to_owned(),
                        &retry_report,
                    )
                    .await
                    .ok();
                let run = record.zip(skill_record).map(|(reflector, skill)| {
                    combined_failed_run(run_id, reflector, skill, &response)
                });
                return Ok(CombinedReviewDispatch::PartialEffect {
                    run,
                    completed_session_reflector: None,
                    run_id: committed_run_id,
                    committed_receipt,
                    detail,
                });
            }
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

    // The reflector is a separately admitted memory operation. Publish its
    // ledger before skill finalization so a later skill failure cannot convert
    // an already committed memory result into a memory PartialEffect.
    let reflector_record = match reflector_finalizer
        .append_success_record(&request, &response, &retry_report, reflector_record)
        .await
    {
        Ok(record) => record,
        Err(error) => {
            if let Some(committed_receipt) = reflector_committed_receipt {
                return Ok(CombinedReviewDispatch::PartialEffect {
                    run: None,
                    completed_session_reflector: None,
                    run_id: reflector_run_id,
                    committed_receipt,
                    detail: "Automatic facts committed, but the memory automation ledger could not be published; reconcile the committed receipt before another run.",
                });
            }
            return Err(error);
        }
    };
    let memory_run = SessionReflectorAutomationRun {
        run_id: reflector_run_id,
        report: reflector_report,
        ledger_record: reflector_record,
        backend_response: Some(response.clone()),
        committed_receipt: reflector_committed_receipt,
    };

    let (skill_report, skill_record, skill_committed_receipt) = match finalize_skill_writer_success(
        &skill_finalizer,
        &skill_bundle.profile_root,
        Some(cg.store_layout().project_root.as_path()),
        config,
        &skill_authority,
        activation_policy,
        ProposedSkillOutput {
            response: &response,
            retry_report: &retry_report,
            evidence: &skill_bundle.evidence,
            evidence_hash: skill_bundle.evidence_hash.clone(),
            proposed_ops: &output,
            proposals: &skills,
        },
        &validation_repairs,
    )
    .await
    {
        Ok(SkillWriterFinalization::Completed {
            report,
            record,
            committed_receipt,
        }) => (report, record, committed_receipt),
        Ok(SkillWriterFinalization::FailedRecorded {
            error,
            record: skill_record,
        }) => {
            return Ok(CombinedReviewDispatch::MemoryCompletedSkillFailure {
                session_reflector: Box::new(memory_run),
                skill_writer_record: Some(skill_record),
                error,
            });
        }
        Err(AutomationRunError::Runtime(err)) => {
            let skill_writer_record = skill_finalizer
                .append_failed_record(
                    response.model.clone(),
                    skill_bundle.evidence_hash.clone(),
                    Some(combined_skill_failure_projection(&output)),
                    err.to_string(),
                    &retry_report,
                )
                .await
                .ok();
            return Ok(CombinedReviewDispatch::MemoryCompletedSkillFailure {
                session_reflector: Box::new(memory_run),
                skill_writer_record,
                error: err,
            });
        }
        Err(AutomationRunError::PartialEffect {
            run_id,
            committed_receipt,
            detail,
            ..
        }) => {
            return Ok(CombinedReviewDispatch::PartialEffect {
                run: None,
                completed_session_reflector: Some(Box::new(memory_run)),
                run_id,
                committed_receipt,
                detail,
            });
        }
    };
    let skill_record = match skill_finalizer
        .append_success_record(&request, &response, &retry_report, skill_record)
        .await
    {
        Ok(record) => record,
        Err(error) => {
            let Some(committed_receipt) = skill_committed_receipt.clone() else {
                return Ok(CombinedReviewDispatch::MemoryCompletedSkillFailure {
                    session_reflector: Box::new(memory_run),
                    skill_writer_record: None,
                    error,
                });
            };
            return Ok(CombinedReviewDispatch::PartialEffect {
                run: None,
                completed_session_reflector: Some(Box::new(memory_run)),
                run_id: skill_finalizer.run_id().to_owned(),
                committed_receipt,
                detail: "Skill lifecycle changes committed, but their automation terminal could not be published; reconcile the skill receipt before another run.",
            });
        }
    };

    Ok(CombinedReviewDispatch::Ran(Box::new(
        CombinedReviewAutomationRun {
            run_id,
            session_reflector: memory_run,
            skill_writer: SkillWriterAutomationRun {
                run_id: skill_run_id,
                report: skill_report,
                ledger_record: skill_record,
                backend_response: Some(response),
                committed_receipt: skill_committed_receipt,
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
            committed_receipt: None,
        },
        skill_writer: SkillWriterAutomationRun {
            run_id: skill_record.run_id.clone(),
            report: failed_backend_fallback_report(&skill_record),
            ledger_record: skill_record,
            backend_response: Some(response.clone()),
            committed_receipt: None,
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
    let skill_projection = proposed_ops.map(combined_skill_failure_projection);
    let skill_record = skill_finalizer
        .append_failed_record(
            response.model.clone(),
            evidence.skill.evidence_hash.clone(),
            skill_projection,
            err.to_string(),
            retry_report,
        )
        .await?;
    Ok((reflector_record, skill_record))
}

fn combined_reflector_failure_projection(output: &Value) -> Value {
    let facts = output
        .get("facts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    json!({
        "schema_version": 1,
        "proposed": {
            "count": facts.len(),
            "sha256": sha256_json(&json!(facts)),
        },
    })
}

fn combined_skill_failure_projection(output: &Value) -> Value {
    json!({
        "skills": output
            .get("skills")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    })
}

fn build_combined_review_prompt(reflector_evidence: &Value, skill_evidence: &Value) -> String {
    format!(
        "This is a combined TraceDecay self-improvement review covering both session reflection and skill writing in one pass. Return only one JSON object containing both a facts array and a skills array; use an empty array for a part with nothing to propose. Follow each part's instructions exactly.\n\n## Part 1: session reflection\n{}\n\n## Part 2: skill review\n{}",
        build_session_reflector_prompt(reflector_evidence),
        build_skill_writer_prompt(skill_evidence)
    )
}

#[cfg(test)]
mod tests;
