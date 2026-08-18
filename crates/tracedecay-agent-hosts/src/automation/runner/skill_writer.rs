use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::automation::backend::{AgentTaskKind, AgentTaskResponse};
use crate::automation::config::AutomationConfig;
use crate::automation::lifecycle::{
    AgentTaskRunContext, AutomationRunLedgerPublication, AutomationRunPublication,
    AutomationRunSettlementGuard, RetainedAutomationRun,
};
use crate::automation::run_ledger::{AutomationRunLedgerRecord, AutomationTrigger};
use crate::errors::{Result, TraceDecayError};

use super::curation::unpersisted_rejected_parts;
use super::session_reflector::{default_include_recent_sessions, default_recent_sessions_limit};
use super::user_evidence_preflight::preflight_user_skill_writer_evidence;
use super::*;
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillWriterAutomationOptions {
    #[serde(default)]
    pub trigger: AutomationTrigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default = "default_skill_writer_provider")]
    pub provider: String,
    #[serde(default = "default_skill_writer_query")]
    pub query: String,
    #[serde(default = "default_skill_writer_evidence_limit")]
    pub evidence_limit: usize,
    /// When true, include bounded turn-ordered slices of recently active
    /// sessions as a primary evidence channel alongside the keyword grep.
    #[serde(default = "default_include_recent_sessions")]
    pub include_recent_sessions: bool,
    /// How many recently active sessions to replay.
    #[serde(default = "default_recent_sessions_limit")]
    pub recent_sessions_limit: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_root: Option<PathBuf>,
}

pub async fn run_skill_writer_with_backend(
    cg: &TraceDecay,
    config: &AutomationConfig,
    configuration_revision_id: &ConfigurationRevisionId,
    backend: &dyn AgentTaskBackend,
    options: SkillWriterAutomationOptions,
) -> AutomationRunResult<SkillWriterAutomationRun> {
    let retrieval = production_project_automation_retrieval(cg).await;
    run_skill_writer_with_backend_and_retrieval(
        cg,
        config,
        configuration_revision_id,
        backend,
        retrieval.as_ref(),
        options,
    )
    .await
}

/// Runs one already-admitted retained application effect without publishing
/// its ledger terminal ahead of outer settlement. The retained settlement
/// authority must bind and publish the returned exact record.
pub async fn run_skill_writer_with_backend_for_retained_settlement(
    cg: &TraceDecay,
    config: &AutomationConfig,
    configuration_revision_id: &ConfigurationRevisionId,
    backend: &dyn AgentTaskBackend,
    options: SkillWriterAutomationOptions,
) -> RetainedAutomationRun<SkillWriterAutomationRun> {
    let retrieval = production_project_automation_retrieval(cg).await;
    run_skill_writer_with_backend_and_retrieval_for_retained_settlement(
        cg,
        config,
        configuration_revision_id,
        backend,
        retrieval.as_ref(),
        options,
    )
    .await
}

/// Retained-settlement variant that preserves the caller's canonical session
/// retrieval authority instead of silently reopening the production route.
pub async fn run_skill_writer_with_backend_and_retrieval_for_retained_settlement(
    cg: &TraceDecay,
    config: &AutomationConfig,
    configuration_revision_id: &ConfigurationRevisionId,
    backend: &dyn AgentTaskBackend,
    retrieval: &dyn AutomationSessionRetrieval,
    options: SkillWriterAutomationOptions,
) -> RetainedAutomationRun<SkillWriterAutomationRun> {
    let settlement_guard = AutomationRunSettlementGuard::new();
    let result = run_skill_writer_with_backend_and_retrieval_publication(
        cg,
        config,
        configuration_revision_id,
        backend,
        retrieval,
        options,
        AutomationRunPublication {
            ledger: AutomationRunLedgerPublication::DeferredUntilApplicationSettlement,
            settlement_guard: Some(&settlement_guard),
        },
    )
    .await;
    RetainedAutomationRun::new(result, settlement_guard)
}

pub async fn run_skill_writer_with_backend_and_retrieval(
    cg: &TraceDecay,
    config: &AutomationConfig,
    configuration_revision_id: &ConfigurationRevisionId,
    backend: &dyn AgentTaskBackend,
    retrieval: &dyn AutomationSessionRetrieval,
    options: SkillWriterAutomationOptions,
) -> AutomationRunResult<SkillWriterAutomationRun> {
    run_skill_writer_with_backend_and_retrieval_publication(
        cg,
        config,
        configuration_revision_id,
        backend,
        retrieval,
        options,
        AutomationRunPublication {
            ledger: AutomationRunLedgerPublication::Immediate,
            settlement_guard: None,
        },
    )
    .await
}

async fn run_skill_writer_with_backend_and_retrieval_publication(
    cg: &TraceDecay,
    config: &AutomationConfig,
    configuration_revision_id: &ConfigurationRevisionId,
    backend: &dyn AgentTaskBackend,
    retrieval: &dyn AutomationSessionRetrieval,
    options: SkillWriterAutomationOptions,
    publication: AutomationRunPublication<'_>,
) -> AutomationRunResult<SkillWriterAutomationRun> {
    let authority =
        project_curation_authority(cg, "automation:skill-writer", configuration_revision_id)?;
    let sessions_db = project_automation_sessions(cg).await?;
    run_skill_writer_for_store_with_publication(
        SkillWriterStoreRuntime {
            dashboard_root: cg.store_layout().dashboard_root.clone(),
            sessions_db,
            analytics_project_root: Some(cg.project_root()),
            analytics_db: Some(cg.profile_database().as_ref()),
            authority,
        },
        retrieval,
        config,
        backend,
        options,
        None,
        publication,
    )
    .await
}

pub(crate) async fn run_user_skill_writer_with_backend_and_retrieval(
    profile_root: &std::path::Path,
    session_registry: Arc<dyn ProfileRuntime>,
    config: &AutomationConfig,
    configuration_revision_id: &ConfigurationRevisionId,
    backend: &dyn AgentTaskBackend,
    retrieval: &dyn AutomationSessionRetrieval,
    mut options: SkillWriterAutomationOptions,
) -> AutomationRunResult<SkillWriterAutomationRun> {
    options.profile_root = Some(profile_root.to_path_buf());
    let sessions_db = session_registry.profile_sessions().await?;
    let authority = profile_curation_authority(
        session_registry.as_ref(),
        "automation:skill-writer",
        configuration_revision_id,
    )?;
    let prebuilt_evidence =
        match preflight_user_skill_writer_evidence(retrieval, config, options.clone()).await? {
            Some(SkillWriterEvidenceOutcome::Ready(bundle)) => Some(bundle),
            Some(SkillWriterEvidenceOutcome::Skipped {
                reason,
                evidence_hash,
            }) => {
                let run = AgentTaskRunContext::new(
                    user_automation_root(profile_root),
                    sessions_db.clone(),
                    options.run_id.clone(),
                    "skill_writer",
                    options.trigger,
                    config,
                    AgentTaskKind::SkillWriter,
                );
                return Ok(rejected_skill_writer_run(
                    &run,
                    config,
                    reason,
                    evidence_hash,
                ));
            }
            None => None,
        };
    run_skill_writer_for_store(
        SkillWriterStoreRuntime {
            dashboard_root: user_automation_root(profile_root),
            sessions_db,
            analytics_project_root: None,
            analytics_db: None,
            authority,
        },
        retrieval,
        config,
        backend,
        options,
        prebuilt_evidence,
    )
    .await
}

pub(super) struct SkillWriterStoreRuntime<'a> {
    pub(super) dashboard_root: PathBuf,
    pub(super) sessions_db: RegisteredGlobalDbLeaseV1,
    pub(super) analytics_project_root: Option<&'a Path>,
    pub(super) analytics_db: Option<&'a RegisteredGlobalDb>,
    pub(super) authority: CurationApplyAuthorityV1,
}

pub(super) async fn run_skill_writer_for_store(
    runtime: SkillWriterStoreRuntime<'_>,
    retrieval: &dyn AutomationSessionRetrieval,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    options: SkillWriterAutomationOptions,
    prebuilt_evidence: Option<SkillWriterEvidenceBundle>,
) -> AutomationRunResult<SkillWriterAutomationRun> {
    run_skill_writer_for_store_with_publication(
        runtime,
        retrieval,
        config,
        backend,
        options,
        prebuilt_evidence,
        AutomationRunPublication {
            ledger: AutomationRunLedgerPublication::Immediate,
            settlement_guard: None,
        },
    )
    .await
}

async fn run_skill_writer_for_store_with_publication(
    runtime: SkillWriterStoreRuntime<'_>,
    retrieval: &dyn AutomationSessionRetrieval,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    options: SkillWriterAutomationOptions,
    prebuilt_evidence: Option<SkillWriterEvidenceBundle>,
    publication: AutomationRunPublication<'_>,
) -> AutomationRunResult<SkillWriterAutomationRun> {
    let AutomationRunPublication {
        ledger: ledger_publication,
        settlement_guard,
    } = publication;
    let SkillWriterStoreRuntime {
        dashboard_root,
        sessions_db,
        analytics_project_root,
        analytics_db,
        authority,
    } = runtime;
    let mut run = AgentTaskRunContext::new(
        dashboard_root,
        sessions_db,
        options.run_id.clone(),
        "skill_writer",
        options.trigger,
        config,
        AgentTaskKind::SkillWriter,
    )
    .with_ledger_publication(ledger_publication)
    .with_settlement_guard(settlement_guard);
    let _run_lock = match run.gate().await? {
        SchedulerGate::Proceed(lock) => lock,
        SchedulerGate::Skip(reason) => {
            return skipped_skill_writer_run(&run, reason, None)
                .await
                .map_err(Into::into);
        }
    };
    let evidence_bundle = match prebuilt_evidence {
        Some(bundle) => bundle,
        None => match build_skill_writer_evidence(
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
        },
    };
    let SkillWriterEvidenceBundle {
        profile_root,
        evidence,
        evidence_hash,
    } = evidence_bundle;
    // Refresh adoption outcomes of previously activated skills so this run's
    // feedback artifact reports real post-activation quality. Best effort: a
    // stale snapshot must not block skill writing.
    if let Err(err) = crate::automation::outcomes::refresh_skill_outcomes(
        &profile_root,
        &run.dashboard_root,
        current_timestamp(),
    )
    .await
    {
        tracing::warn!(error = %err, "failed to refresh skill outcomes");
    }

    let activation_policy = skill_writer_activation_policy();
    let request = AgentTaskRequest::new(
        run.run_id.clone(),
        AgentTaskKind::SkillWriter,
        build_skill_writer_prompt(&evidence),
        evidence_hash.clone(),
        json!({
            "skill_writer_evidence": evidence,
            "apply": true,
            "activation_policy": activation_policy,
        }),
    );
    let input_hash = Some(request.input_hash.clone());
    let finalizer = run.finalizer(input_hash.clone())?;
    let (mut response, mut retry_report) = match finalizer
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
                committed_receipt: None,
            });
        }
    };
    let (mut proposed_ops, mut proposals) = finalizer
        .response_output_array(
            &response,
            evidence_hash.clone(),
            &retry_report,
            "skills",
            "skill writer output must include a skills array",
        )
        .await?;
    let mut validation_repairs = Vec::new();
    for attempt in 1..=2 {
        let validation_errors =
            validate_skill_proposals(&profile_root, &run.run_id, &proposals).await?;
        if validation_errors.is_empty() {
            break;
        }
        validation_repairs.push(json!({
            "attempt": attempt,
            "errors": validation_errors,
        }));
        if attempt == 2 {
            let error = TraceDecayError::Config {
                message: "skill proposal validation repair budget exhausted; output quarantined"
                    .to_string(),
            };
            let ledger_record = finalizer
                .append_failed_record(
                    response.model.clone(),
                    evidence_hash,
                    Some(proposed_ops),
                    error.to_string(),
                    &retry_report,
                )
                .await?;
            return Err(AutomationRunError::RecordedFailure {
                error,
                ledger_record: Box::new(ledger_record),
            });
        }
        let repair_request = AgentTaskRequest::new(
            run.run_id.clone(),
            AgentTaskKind::SkillWriter,
            format!(
                "Repair the previous skill proposal JSON. Return only {{\"skills\": [...]}}. Preserve valid intent, fix every validation error, and do not add unrelated changes.\n{}",
                serde_json::to_string_pretty(validation_repairs.last().unwrap_or(&Value::Null))
                    .map_err(TraceDecayError::from)?
            ),
            evidence_hash.clone(),
            json!({
                "previous_output": proposed_ops.clone(),
                "validation_errors": validation_repairs.last(),
                "activation_policy": activation_policy,
            }),
        );
        let repair_policy = BackendRetryPolicy::from_timeout_secs(config.timeout_secs);
        let mut repair_retry_report = AgentTaskRetryReport::default();
        response = match run_agent_task_with_retry_report(
            backend,
            &repair_request,
            &repair_policy,
            &mut repair_retry_report,
        )
        .await
        {
            Ok(response) => response,
            Err(error) => {
                retry_report.append(repair_retry_report);
                let ledger_record = finalizer
                    .append_failed_record(
                        None,
                        evidence_hash,
                        Some(proposed_ops),
                        error.to_string(),
                        &retry_report,
                    )
                    .await?;
                return Err(AutomationRunError::RecordedFailure {
                    error,
                    ledger_record: Box::new(ledger_record),
                });
            }
        };
        retry_report.append(repair_retry_report);
        (proposed_ops, proposals) = finalizer
            .response_output_array(
                &response,
                evidence_hash.clone(),
                &retry_report,
                "skills",
                "skill writer repair output must include a skills array",
            )
            .await?;
    }
    let (report, record, committed_receipt) = match finalize_skill_writer_success(
        &finalizer,
        &profile_root,
        analytics_project_root,
        config,
        &authority,
        activation_policy,
        ProposedSkillOutput {
            response: &response,
            retry_report: &retry_report,
            evidence: &evidence,
            evidence_hash: evidence_hash.clone(),
            proposed_ops: &proposed_ops,
            proposals: &proposals,
            validation_repairs: &validation_repairs,
        },
    )
    .await
    {
        Ok(SkillWriterFinalization::Completed {
            report,
            record,
            committed_receipt,
        }) => (report, record, committed_receipt.map(|receipt| *receipt)),
        Ok(SkillWriterFinalization::FailedRecorded { error, record }) => {
            return Err(AutomationRunError::RecordedFailure {
                error,
                ledger_record: Box::new(record),
            });
        }
        Err(AutomationRunError::Runtime(err)) => {
            let ledger_record = finalizer
                .append_failed_record(
                    response.model.clone(),
                    evidence_hash,
                    Some(proposed_ops),
                    err.to_string(),
                    &retry_report,
                )
                .await?;
            return Err(AutomationRunError::RecordedFailure {
                error: err,
                ledger_record: Box::new(ledger_record),
            });
        }
        Err(error @ AutomationRunError::RecordedFailure { .. }) => return Err(error),
        Err(error @ AutomationRunError::PartialEffect { .. }) => return Err(error),
    };
    let record = finalizer
        .append_success_record(&request, &response, &retry_report, record)
        .await
        .map_err(|error| match committed_receipt.clone() {
            Some(committed_receipt) => AutomationRunError::PartialEffect {
                run_id: run.run_id.clone(),
                committed_receipt: Box::new(committed_receipt),
                ledger_record: None,
                detail: "Skill lifecycle changes committed, but their automation terminal could not be published; reconcile the skill receipt before another run.",
            },
            None => AutomationRunError::Runtime(error),
        })?;

    Ok(SkillWriterAutomationRun {
        run_id: run.run_id,
        report,
        ledger_record: record,
        backend_response: Some(response),
        committed_receipt,
    })
}

/// Validates and automatically applies the `skills` half of a skill-writer (or
/// combined) run, returning the report plus the not-yet-appended ledger record.
pub(super) struct ProposedSkillOutput<'a> {
    pub(super) response: &'a AgentTaskResponse,
    pub(super) retry_report: &'a AgentTaskRetryReport,
    pub(super) evidence: &'a Value,
    pub(super) evidence_hash: Option<String>,
    pub(super) proposed_ops: &'a Value,
    pub(super) proposals: &'a [Value],
    pub(super) validation_repairs: &'a [Value],
}

fn skill_validation_repairs_summary(validation_repairs: &[Value]) -> Value {
    json!({
        "count": validation_repairs.len(),
        "sha256": sha256_json(&json!(validation_repairs)),
    })
}

pub(super) async fn finalize_skill_writer_success(
    finalizer: &AgentRunFinalizer<'_>,
    profile_root: &std::path::Path,
    project_root: Option<&std::path::Path>,
    config: &AutomationConfig,
    authority: &CurationApplyAuthorityV1,
    activation_policy: &'static str,
    output: ProposedSkillOutput<'_>,
) -> AutomationRunResult<SkillWriterFinalization> {
    let ProposedSkillOutput {
        response,
        retry_report,
        evidence,
        evidence_hash,
        proposed_ops,
        proposals,
        validation_repairs,
    } = output;
    let run_id = finalizer.run_id();
    let curation_decision =
        evaluate_skill_curation(config, authority, evidence_hash.as_deref(), proposals)?;
    let proposal_outcome = validate_and_apply_skill_proposals(
        profile_root,
        project_root,
        run_id,
        proposals,
        &curation_decision,
    )
    .await?;
    let accepted_count = proposal_outcome.created.len()
        + proposal_outcome.updated.len()
        + proposal_outcome.consolidations.len();
    let rejected_count = proposal_outcome.rejected.len();
    let committed_receipt = (accepted_count > 0).then(|| {
        let deployment = match proposal_outcome
            .deployment
            .as_ref()
            .map(|receipt| receipt.status)
        {
            None => ExternalSkillDeploymentDisposition::NotRequired,
            Some(crate::automation::skill_writer::ManagedSkillDeploymentStatus::Complete) => {
                ExternalSkillDeploymentDisposition::Complete
            }
            Some(crate::automation::skill_writer::ManagedSkillDeploymentStatus::PartialFailure) => {
                ExternalSkillDeploymentDisposition::PartialFailure
            }
            Some(crate::automation::skill_writer::ManagedSkillDeploymentStatus::Unavailable) => {
                ExternalSkillDeploymentDisposition::Unavailable
            }
        };
        crate::automation::jobs::effect_receipt::skill_writing_receipt(
            run_id,
            proposal_outcome.created.len(),
            proposal_outcome.updated.len(),
            proposal_outcome.consolidations.len(),
            deployment,
            sha256_json(&json!({
                "created": &proposal_outcome.created,
                "updated": &proposal_outcome.updated,
                "consolidations": &proposal_outcome.consolidations,
                "deployment": &proposal_outcome.deployment,
            })),
        )
    });
    let completed_at_micros = finalizer
        .completion_timestamp_micros()
        .map_err(|error| match committed_receipt.clone() {
            Some(committed_receipt) => AutomationRunError::PartialEffect {
                run_id: run_id.to_owned(),
                committed_receipt: Box::new(committed_receipt),
                ledger_record: None,
                detail: "Skill lifecycle changes committed, but their exact completion time could not be recorded; reconcile the skill receipt before another run.",
            },
            None => AutomationRunError::Runtime(error),
        })?;
    let deployment_failed = proposal_outcome
        .deployment
        .as_ref()
        .is_some_and(|deployment| deployment.retry_required);
    let no_candidate = proposals.is_empty();
    let fully_applied = !no_candidate
        && accepted_count == proposals.len()
        && rejected_count == 0
        && !deployment_failed;
    let report = json!({
        "status": if no_candidate {
            "no_candidate"
        } else if fully_applied {
            "applied"
        } else {
            "failed_after_partial_effects"
        },
        "dry_run": false,
        "task": "skill_writer",
        "evidence_hash": evidence_hash,
        "activation_policy": activation_policy,
        "curation_policy": {
            "decision": curation_decision,
            "effect": {
                "accepted_count": accepted_count,
                "rejected_count": rejected_count,
                "fully_applied": fully_applied,
                "mutates_store": accepted_count > 0,
            },
        },
        "created_skills": proposal_outcome.created,
        "updated_skills": proposal_outcome.updated,
        "applied_consolidations": proposal_outcome.consolidations,
        "rejected_skills": proposal_outcome.rejected,
        "deployment": proposal_outcome.deployment,
        "validation_repairs": validation_repairs,
        "skill_improvement_recommendations": evidence
            .get("skill_improvement_recommendations")
            .cloned()
            .unwrap_or_else(|| json!([])),
    });
    if !no_candidate && !fully_applied {
        let error = TraceDecayError::Config {
            message: if deployment_failed {
                "skill curation applied lifecycle changes but host deployment requires retry"
                    .to_string()
            } else {
                "skill curation could not apply every validated proposal".to_string()
            },
        };
        let mut record = finalizer.success_record_at(
            response,
            evidence_hash,
            Some(
                json!({"skills": proposed_ops.get("skills").cloned().unwrap_or_else(|| json!([]))}),
            ),
            accepted_count,
            rejected_count,
            completed_at_micros,
        );
        record.status = crate::automation::run_ledger::AutomationRunStatus::Failed;
        record.error = Some(error.to_string());
        record.error_classification =
            Some(crate::automation::backend::AgentTaskFailureClass::Permanent);
        record.error_retryable = Some(false);
        record.applied_ops = Some(json!({
            "created_skills": report.get("created_skills").cloned().unwrap_or_else(|| json!([])),
            "updated_skills": report.get("updated_skills").cloned().unwrap_or_else(|| json!([])),
            "applied_consolidations": report.get("applied_consolidations").cloned().unwrap_or_else(|| json!([])),
            "deployment": report.get("deployment").cloned().unwrap_or(Value::Null),
        }));
        record.rejected_ops = report.get("rejected_skills").cloned();
        record.validation_report = Some(json!({
            "status": "failed_after_partial_effects",
            "validation_repairs": skill_validation_repairs_summary(validation_repairs),
            "curation_policy": report.get("curation_policy").cloned().unwrap_or_else(|| json!({})),
            "deployment": report.get("deployment").cloned().unwrap_or(Value::Null),
        }));
        let record = finalizer
            .append_prebuilt_failed_record(record, retry_report)
            .await
            .map_err(|error| match committed_receipt.clone() {
                Some(committed_receipt) => AutomationRunError::PartialEffect {
                    run_id: run_id.to_owned(),
                    committed_receipt: Box::new(committed_receipt),
                    ledger_record: None,
                    detail: "Skill lifecycle changes committed, but their failed automation terminal could not be published; reconcile the skill receipt before another run.",
                },
                None => AutomationRunError::Runtime(error),
            })?;
        if let Some(committed_receipt) = committed_receipt {
            return Err(AutomationRunError::PartialEffect {
                run_id: run_id.to_owned(),
                committed_receipt: Box::new(committed_receipt),
                ledger_record: Some(Box::new(record)),
                detail: "Skill lifecycle changes committed, but the batch did not reach complete success; reconcile the skill receipt before another run.",
            });
        }
        return Ok(SkillWriterFinalization::FailedRecorded { error, record });
    }
    let mut record = finalizer.success_record_at(
        response,
        report
            .get("evidence_hash")
            .and_then(Value::as_str)
            .map(str::to_string),
        Some(json!({
            "skills": proposed_ops.get("skills").cloned().unwrap_or_else(|| json!([])),
            "created_skills": report.get("created_skills").cloned().unwrap_or_else(|| json!([])),
            "updated_skills": report.get("updated_skills").cloned().unwrap_or_else(|| json!([])),
            "applied_consolidations": report.get("applied_consolidations").cloned().unwrap_or_else(|| json!([])),
            "rejected_skills": report.get("rejected_skills").cloned().unwrap_or_else(|| json!([])),
            "deployment": report.get("deployment").cloned().unwrap_or(Value::Null),
        })),
        accepted_count,
        rejected_count,
        completed_at_micros,
    );
    record.applied_ops = (accepted_count > 0).then(|| {
        json!({
            "created_skills": report.get("created_skills").cloned().unwrap_or_else(|| json!([])),
            "updated_skills": report.get("updated_skills").cloned().unwrap_or_else(|| json!([])),
            "applied_consolidations": report.get("applied_consolidations").cloned().unwrap_or_else(|| json!([])),
            "deployment": report.get("deployment").cloned().unwrap_or(Value::Null),
        })
    });
    record.rejected_ops = report.get("rejected_skills").cloned();
    record.validation_report = Some(json!({
        "status": report.get("status").cloned().unwrap_or_else(|| json!("applied")),
        "dry_run": false,
        "activation_policy": activation_policy,
        "accepted_count": accepted_count,
        "rejected_count": rejected_count,
        "validation_repairs": skill_validation_repairs_summary(validation_repairs),
        "curation_policy": report.get("curation_policy").cloned().unwrap_or_else(|| json!({})),
    }));
    Ok(SkillWriterFinalization::Completed {
        report,
        record,
        committed_receipt: committed_receipt.map(Box::new),
    })
}

impl Default for SkillWriterAutomationOptions {
    fn default() -> Self {
        Self {
            trigger: AutomationTrigger::ManualCli,
            run_id: None,
            provider: default_skill_writer_provider(),
            query: default_skill_writer_query(),
            evidence_limit: default_skill_writer_evidence_limit(),
            include_recent_sessions: default_include_recent_sessions(),
            recent_sessions_limit: default_recent_sessions_limit(),
            profile_root: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillWriterAutomationRun {
    pub run_id: String,
    pub report: Value,
    pub ledger_record: AutomationRunLedgerRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_response: Option<AgentTaskResponse>,
    #[serde(skip)]
    pub committed_receipt: Option<AutomationCommittedReceipt>,
}

pub(super) enum SkillWriterFinalization {
    Completed {
        report: Value,
        record: AutomationRunLedgerRecord,
        committed_receipt: Option<Box<AutomationCommittedReceipt>>,
    },
    FailedRecorded {
        error: TraceDecayError,
        record: AutomationRunLedgerRecord,
    },
}

pub(super) fn default_skill_writer_provider() -> String {
    "all".to_string()
}

pub(super) fn default_skill_writer_query() -> String {
    "workflow correction repeated skill tool pattern".to_string()
}

fn default_skill_writer_evidence_limit() -> usize {
    20
}

pub(super) fn build_skill_writer_prompt(evidence: &Value) -> String {
    const POLICY: &str = concat!(
        "Review these bounded TraceDecay session snippets and propose only reusable managed skills for repeated workflows, corrections, or tool-use patterns.\n",
        "Evidence has two channels: recent_session_slices holds turn-ordered head/tail turns and summary nodes replayed from recently active sessions, and hits holds keyword search matches.\n",
        "\n",
        "Target shape of the skill library: CLASS-LEVEL umbrella skills, each with a rich body and support files for session-specific detail — not a long flat list of narrow one-session-one-skill entries. This shapes HOW you update, not WHETHER you update.\n",
        "\n",
        "Signals that warrant a skill proposal (any one is enough):\n",
        "- The user corrected the agent's style, tone, format, verbosity, workflow, or approach. Frustration signals like 'stop doing X', 'this is too verbose', 'don't format like this', 'you always do Y and I hate it', or an explicit 'remember this' are FIRST-CLASS skill signals, not just memory signals. Embed the correction in the body of the skill that governs that class of task so the next session starts already knowing; a memory fact alone is not enough.\n",
        "- A non-trivial technique, fix, workaround, debugging path, or tool-usage pattern emerged that a future session would benefit from.\n",
        "- A skill that evidence shows was used or loaded this session turned out to be wrong, missing a step, or outdated. Patch it now.\n",
        "\n",
        "Preference order — pick the EARLIEST action that fits:\n",
        "1. UPDATE a skill that the evidence (skill_usage_summaries, skill_improvement_recommendations, existing_managed_skills) shows was used or loaded recently. It was in play, so it is the right one to extend.\n",
        "2. PATCH an existing umbrella skill from existing_managed_skills whose class covers the new learning. Add a subsection, a pitfall, or broaden a trigger.\n",
        "3. ADD to an existing skill's scope via its support_files (reference notes, templates, or re-runnable snippets), with a one-line pointer in the skill body so future sessions find it.\n",
        "4. CREATE a new skill only when nothing existing fits. The name MUST be at the class level and MUST survive the test: 'does this name only make sense for today's task?' If yes, it is wrong — no PR numbers, error strings, feature codenames, or fix-X/debug-Y session artifacts. Fall back to option 1, 2, or 3 instead.\n",
        "\n",
        "Do NOT capture (these become persistent self-imposed constraints that bite later when the environment changes):\n",
        "- Environment-dependent failures: missing binaries, 'command not found', unconfigured credentials, uninstalled packages, post-migration path mismatches. The user can fix these; they are not durable rules.\n",
        "- Negative claims about tools or features ('X is broken', 'browser tools do not work'). These harden into refusals the agent cites against itself long after the actual problem was fixed. If a tool failed because of setup state, capture the FIX (install command, config step, env var) under an existing setup or troubleshooting skill — never 'this tool does not work' as a standalone constraint.\n",
        "- Session-specific transient errors that resolved before the session ended. If retrying worked, the lesson is the retry pattern, not the original failure.\n",
        "- One-off task narratives. A single 'summarize this' or 'analyze this PR' request is not a class of work that warrants a skill.\n",
        "- Secrets, credentials, or tokens in any skill body or support file.\n",
        "\n",
        "An empty skills array is a real option when the session ran smoothly with no corrections and produced no new technique, but do not reach for it as a default.\n",
        "\n",
        "Response contract: Return only JSON with a skills array of managed skill creates or updates. New skills may omit action or use action=create and must include id, title, summary, category, body_markdown, optional targets, optional support_files with text content, and reason. Targets, when present, must be an array using cursor, codex, claude, agents, opencode, kimi, kiro, or hermes; Hermes exports are generated read-only under the TraceDecay plugin package and never overwrite host-owned user skills. Updates must use action=update or action=patch, include id and base_checksum, and include at least one changed field among title, summary, category, targets, body_markdown/body, support_files, or pinned. For updates, support_files is a complete replacement list, not a partial file patch. Consolidations: when skill_overlap_candidates shows overlapping managed skills, you may propose action=merge (include id for the surviving skill, base_checksum, source_skill_id, source_base_checksum, reason, and optional merged title/summary/category/targets/body_markdown/support_files) or action=archive (include id, base_checksum, reason). Consolidations preserve archived source content. Valid proposals are activated and exported automatically. Never propose merge or archive for pinned or user-authored skills.\n",
    );
    format!(
        "{POLICY}{}",
        serde_json::to_string_pretty(evidence).unwrap_or_else(|_| "{}".to_string())
    )
}

pub(super) async fn skipped_skill_writer_run(
    run: &AgentTaskRunContext<'_>,
    reason: &str,
    evidence_hash: Option<String>,
) -> Result<SkillWriterAutomationRun> {
    let (report, record) = run
        .skipped_parts(evidence_hash, reason, Some("skill_writer"))
        .await?;
    Ok(SkillWriterAutomationRun {
        run_id: run.run_id.clone(),
        report,
        ledger_record: record,
        backend_response: None,
        committed_receipt: None,
    })
}

pub(super) fn rejected_skill_writer_run(
    run: &AgentTaskRunContext<'_>,
    config: &AutomationConfig,
    reason: &str,
    evidence_hash: Option<String>,
) -> SkillWriterAutomationRun {
    let (report, record) = unpersisted_rejected_parts(
        run,
        config,
        AgentTaskKind::SkillWriter,
        reason,
        evidence_hash,
        "skill_writer",
    );
    SkillWriterAutomationRun {
        run_id: run.run_id.clone(),
        report,
        ledger_record: record,
        backend_response: None,
        committed_receipt: None,
    }
}
