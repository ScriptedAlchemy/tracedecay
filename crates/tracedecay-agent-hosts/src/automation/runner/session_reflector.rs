use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use std::path::PathBuf;
use std::sync::Arc;

use tracedecay_domain::FactOwnerV1;
use tracedecay_store::ProjectMemoryFactStore;

use super::user_automation_root;
use crate::automation::backend::{
    AgentTaskBackend, AgentTaskKind, AgentTaskRequest, AgentTaskResponse, AgentTaskRetryReport,
};
use crate::automation::config::AutomationConfig;
use crate::automation::fact_proposals::{
    FactProposalRecord, FactProposalState, apply_fact_proposal_with_result,
    record_session_fact_proposals,
};
use crate::automation::lifecycle::{
    AgentRunFinalizer, AgentTaskRunContext, BackendTaskRun, SchedulerGate,
    failed_backend_fallback_report, task_skip_reason,
};
use crate::automation::run_ledger::{AutomationRunLedgerRecord, AutomationTrigger};
use crate::automation::session_reflector::validate_fact_proposals;
use crate::errors::{Result, TraceDecayError};
use crate::ports::project_runtime::ProfileRuntime;
use crate::ports::project_runtime::TraceDecay;
use crate::ports::session_evidence::{LcmGrepSort, LcmScope};
use crate::store::memory::DatabaseFactStore;
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::tracedecay::current_timestamp;
use tracedecay_usecases::memory::MemoryApplication;

use super::curation::{evaluate_session_curation, unpersisted_rejected_parts};

use super::evidence::{
    SessionReflectorEvidenceBundle, SessionReflectorEvidenceOutcome,
    build_session_reflector_evidence,
};
use super::retrieval::{AutomationSessionRetrieval, production_project_automation_retrieval};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionReflectorAutomationOptions {
    #[serde(default)]
    pub trigger: AutomationTrigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default = "default_session_provider")]
    pub provider: String,
    #[serde(default = "default_session_reflection_query")]
    pub query: String,
    #[serde(default = "default_lcm_grep_scope")]
    pub scope: LcmScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default = "default_include_summaries")]
    pub include_summaries: bool,
    #[serde(default = "default_session_evidence_limit")]
    pub evidence_limit: usize,
    /// When true, include bounded turn-ordered slices of recently active
    /// sessions as a primary evidence channel alongside the keyword grep.
    #[serde(default = "default_include_recent_sessions")]
    pub include_recent_sessions: bool,
    /// How many recently active sessions to replay when `session_id` is not
    /// explicitly set.
    #[serde(default = "default_recent_sessions_limit")]
    pub recent_sessions_limit: usize,
    #[serde(default = "default_lcm_grep_sort")]
    pub sort: LcmGrepSort,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_time: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_time: Option<i64>,
}

impl Default for SessionReflectorAutomationOptions {
    fn default() -> Self {
        Self {
            trigger: AutomationTrigger::ManualCli,
            run_id: None,
            provider: default_session_provider(),
            query: default_session_reflection_query(),
            scope: default_lcm_grep_scope(),
            session_id: None,
            include_summaries: default_include_summaries(),
            evidence_limit: default_session_evidence_limit(),
            include_recent_sessions: default_include_recent_sessions(),
            recent_sessions_limit: default_recent_sessions_limit(),
            sort: default_lcm_grep_sort(),
            source: None,
            role: None,
            start_time: None,
            end_time: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionReflectorAutomationRun {
    pub run_id: String,
    pub report: Value,
    pub ledger_record: AutomationRunLedgerRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_response: Option<AgentTaskResponse>,
}

pub(super) fn default_session_provider() -> String {
    "cursor".to_string()
}

fn default_lcm_grep_scope() -> LcmScope {
    LcmScope::All
}

fn default_include_summaries() -> bool {
    true
}

fn default_lcm_grep_sort() -> LcmGrepSort {
    LcmGrepSort::Recency
}

pub(super) fn default_session_reflection_query() -> String {
    "remember prefer decision requirement workflow".to_string()
}

fn default_session_evidence_limit() -> usize {
    20
}

pub(super) fn default_include_recent_sessions() -> bool {
    true
}

pub(super) fn default_recent_sessions_limit() -> usize {
    3
}

pub(super) fn build_session_reflector_prompt(evidence: &Value) -> String {
    const POLICY: &str = concat!(
        "Review these bounded TraceDecay session snippets and propose only durable memory facts.\n",
        "Evidence has two channels: recent_session_slices holds turn-ordered head/tail turns and summary nodes replayed from recently active sessions, and hits holds keyword search matches; both are citable.\n",
        "\n",
        "Signals worth capturing (any one is enough):\n",
        "- The user revealed durable preferences, persona, expectations, or ways they want the agent to operate.\n",
        "- The user corrected the agent's style, tone, format, verbosity, workflow, or approach. Frustration signals like 'stop doing X', 'this is too verbose', 'don't format like this', or an explicit 'remember this' are FIRST-CLASS signals: capture the correction as a durable user_pref or decision fact so the next session starts already knowing. These corrections should also end up embedded in the skill that governs that class of task, not only in memory; the skill writer handles the skill side, but the fact must still be recorded here.\n",
        "- A durable project, tool, decision, or code-area fact emerged that a future session would need.\n",
        "\n",
        "Do NOT capture (these harden into stale or self-defeating rules):\n",
        "- Environment-dependent failures: missing binaries, 'command not found', unconfigured credentials, uninstalled packages, post-migration path mismatches. The user can fix these; they are not durable facts.\n",
        "- Negative claims about tools or features ('X is broken', 'Y does not work'). These harden into self-imposed refusals cited long after the actual problem was fixed. If a tool failed because of setup state, the durable fact is the FIX (install command, config step, env var), never 'this tool does not work'.\n",
        "- Session-specific transient errors that resolved before the session ended. If retrying worked, the lesson is the retry pattern, not the original failure.\n",
        "- One-off task narratives. A single 'summarize this' or 'analyze this PR' request is not a durable fact about the user or project.\n",
        "- Secrets, credentials, tokens, or ephemeral status.\n",
        "\n",
        "Proposing nothing is a real option when the session ran smoothly and revealed nothing durable, but do not reach for it as a default.\n",
        "\n",
        "Response contract: Return only JSON with a facts array. Each fact must include content, category, optional tags, optional entities, trust, source_span, and reason. Category must be one of general, user_pref, project, tool, decision, or code_area. Use trust, not confidence; trust must be a JSON number from 0.0 to 1.0. Do not use string labels like high, medium, or low. source_span must cite one bounded evidence hit by session_id plus message_id for raw messages, by store_id for raw messages, or by node_id for summaries. Do not include secrets or ephemeral status.\n",
    );
    format!(
        "{POLICY}{}",
        serde_json::to_string_pretty(evidence).unwrap_or_else(|_| "{}".to_string())
    )
}

pub(super) async fn validate_session_fact_proposals<A: ProjectMemoryFactStore>(
    memory: &MemoryApplication<A>,
    proposals: &[Value],
    evidence: &Value,
) -> Result<(Vec<Value>, Vec<Value>)> {
    validate_fact_proposals(memory, proposals, evidence).await
}

pub(super) async fn auto_apply_session_fact_proposals<A: ProjectMemoryFactStore>(
    memory: &MemoryApplication<A>,
    proposal_records: Vec<FactProposalRecord>,
) -> AutoApplySessionFactProposals {
    let mut applied = Vec::with_capacity(proposal_records.len());
    let mut newly_promoted = false;
    for record in proposal_records {
        if record.add_fact_request.is_none() || record.validation_reason.is_some() {
            applied.push(record);
            continue;
        }
        let result = match apply_fact_proposal_with_result(
            memory,
            &record.proposal_id,
            Some("session_reflector:auto_apply".to_string()),
        )
        .await
        {
            Ok(result) => result,
            Err(error) => {
                return AutoApplySessionFactProposals {
                    records: applied,
                    newly_promoted,
                    error: Some(error),
                };
            }
        };
        newly_promoted |= result.newly_promoted;
        applied.push(result.record);
    }
    AutoApplySessionFactProposals {
        records: applied,
        newly_promoted,
        error: None,
    }
}

struct AutoApplySessionFactProposals {
    records: Vec<FactProposalRecord>,
    newly_promoted: bool,
    error: Option<TraceDecayError>,
}

async fn skipped_session_reflector_run(
    run: &AgentTaskRunContext<'_>,
    reason: &str,
    evidence_hash: Option<String>,
) -> Result<SessionReflectorAutomationRun> {
    let (report, record) = run
        .skipped_parts(evidence_hash, reason, Some("session_reflector"))
        .await?;
    Ok(SessionReflectorAutomationRun {
        run_id: run.run_id.clone(),
        report,
        ledger_record: record,
        backend_response: None,
    })
}

fn rejected_session_reflector_run(
    run: &AgentTaskRunContext<'_>,
    config: &AutomationConfig,
    reason: &str,
    evidence_hash: Option<String>,
) -> SessionReflectorAutomationRun {
    let (report, record) = unpersisted_rejected_parts(
        run,
        config,
        AgentTaskKind::SessionReflector,
        reason,
        evidence_hash,
        "session_reflector",
    );
    SessionReflectorAutomationRun {
        run_id: run.run_id.clone(),
        report,
        ledger_record: record,
        backend_response: None,
    }
}

/// Validates and automatically applies the `facts` half of a reflector (or
/// combined) run, returning the report plus the not-yet-appended ledger record.
pub(super) struct ProposedAgentOutput<'a> {
    pub(super) response: &'a AgentTaskResponse,
    pub(super) retry_report: &'a AgentTaskRetryReport,
    pub(super) evidence: &'a Value,
    pub(super) evidence_hash: Option<String>,
    pub(super) proposed_ops: &'a Value,
    pub(super) proposals: &'a [Value],
}

pub(super) enum SessionReflectorFinalization {
    Completed {
        report: Value,
        record: AutomationRunLedgerRecord,
    },
    FailedRecorded {
        error: TraceDecayError,
        record: AutomationRunLedgerRecord,
    },
}

pub(super) async fn finalize_session_reflector_success<A: ProjectMemoryFactStore>(
    memory: &MemoryApplication<A>,
    config: &AutomationConfig,
    finalizer: &AgentRunFinalizer<'_>,
    output: ProposedAgentOutput<'_>,
    validation_repairs: &[Value],
) -> Result<SessionReflectorFinalization> {
    let ProposedAgentOutput {
        response,
        retry_report,
        evidence,
        evidence_hash,
        proposed_ops,
        proposals,
    } = output;
    let run_id = finalizer.run_id();
    let (accepted_facts, rejected_facts) =
        validate_session_fact_proposals(memory, proposals, evidence).await?;
    let accepted_count = accepted_facts.len();
    let rejected_count = rejected_facts.len();
    let proposal_records = record_session_fact_proposals(
        memory,
        run_id,
        evidence_hash.as_deref(),
        &accepted_facts,
        &rejected_facts,
    )
    .await?;
    let curation_decision =
        evaluate_session_curation(config, evidence_hash.as_deref(), &accepted_facts)?;
    let proposal_records = if curation_decision.allows_apply() {
        let auto_apply = auto_apply_session_fact_proposals(memory, proposal_records).await;
        if let Some(error) = auto_apply.error {
            let record = finalizer
                .append_failed_record_with_effects(
                    response.model.clone(),
                    evidence_hash,
                    Some(json!({
                        "facts": proposed_ops.get("facts").cloned().unwrap_or_else(|| json!([])),
                        "accepted_facts": accepted_facts,
                        "rejected_facts": rejected_facts,
                    })),
                    format!("session fact auto-apply failed after partial effects: {error}"),
                    retry_report,
                    Some(json!({
                        "proposal_records": auto_apply.records,
                        "newly_promoted": auto_apply.newly_promoted,
                    })),
                    None,
                    Some(json!({
                        "status": "failed_after_partial_effects",
                        "error": error.to_string(),
                    })),
                    accepted_count,
                    rejected_count,
                )
                .await?;
            return Ok(SessionReflectorFinalization::FailedRecorded { error, record });
        }
        auto_apply.records
    } else {
        proposal_records
    };
    let proposal_ids: Vec<String> = proposal_records
        .iter()
        .map(|record| record.proposal_id.clone())
        .collect();
    let applied_proposal_ids: Vec<String> = proposal_records
        .iter()
        .filter(|record| record.state == FactProposalState::Applied)
        .map(|record| record.proposal_id.clone())
        .collect();
    let applied_fact_ids: Vec<String> = proposal_records
        .iter()
        .filter(|record| record.state == FactProposalState::Applied)
        .filter_map(|record| record.applied_fact_id.clone())
        .collect();
    let applied_count = applied_proposal_ids.len();
    let fully_applied = accepted_count > 0 && applied_count == accepted_count;
    let curation_policy = json!({
        "decision": curation_decision,
        "effect": {
            "accepted_count": accepted_count,
            "applied_proposal_ids": applied_proposal_ids,
            "applied_fact_ids": applied_fact_ids,
            "applied_count": applied_count,
            "fully_applied": fully_applied,
            "mutates_store": applied_count > 0,
        },
    });
    let report = json!({
        "status": if accepted_count == 0 {
            "no_valid_facts"
        } else if curation_decision.allows_apply() {
            "auto_applied"
        } else {
            "curation_not_applied"
        },
        "dry_run": false,
        "task": "session_reflector",
        "evidence_hash": evidence_hash,
        "accepted_facts": accepted_facts,
        "rejected_facts": rejected_facts,
        "proposal_ids": proposal_ids,
        "proposal_records": proposal_records,
        "curation_policy": curation_policy,
        "validation_repairs": validation_repairs,
    });
    let mut record = finalizer.success_record(
        response,
        report
            .get("evidence_hash")
            .and_then(Value::as_str)
            .map(str::to_string),
        Some(json!({
            "facts": proposed_ops.get("facts").cloned().unwrap_or_else(|| json!([])),
            "accepted_facts": report.get("accepted_facts").cloned().unwrap_or_else(|| json!([])),
            "rejected_facts": report.get("rejected_facts").cloned().unwrap_or_else(|| json!([])),
            "proposal_ids": report.get("proposal_ids").cloned().unwrap_or_else(|| json!([])),
        })),
        accepted_count,
        rejected_count,
    );
    record.backend_attempt_count = retry_report.attempt_count();
    record.backend_attempts = retry_report.attempts().to_vec();
    record.applied_ops = report
        .pointer("/curation_policy/effect/applied_proposal_ids")
        .filter(|value| value.as_array().is_some_and(|items| !items.is_empty()))
        .cloned();
    record.rejected_ops = report.get("rejected_facts").cloned();
    let applied_proposal_ids = report
        .pointer("/curation_policy/effect/applied_proposal_ids")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let mut validation_report = json!({
        "status": report.get("status").cloned().unwrap_or_else(|| json!("no_valid_facts")),
        "dry_run": report.get("dry_run").cloned().unwrap_or(json!(false)),
        "accepted_count": accepted_count,
        "rejected_count": rejected_count,
        "validation_repairs": validation_repairs,
        "curation_policy": report.get("curation_policy").cloned().unwrap_or_else(|| json!({})),
    });
    if let Some(object) = validation_report.as_object_mut() {
        object.insert(
            "applied_proposals".to_string(),
            json!({
            "proposal_ids": applied_proposal_ids,
            "accepted_facts": report.get("accepted_facts").cloned().unwrap_or_else(|| json!([])),
            }),
        );
    }
    record.validation_report = Some(validation_report);
    Ok(SessionReflectorFinalization::Completed { report, record })
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_session_reflector_for_store<A: ProjectMemoryFactStore>(
    dashboard_root: PathBuf,
    sessions_db: Arc<RegisteredGlobalDb>,
    retrieval: &dyn AutomationSessionRetrieval,
    memory: &MemoryApplication<A>,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    options: SessionReflectorAutomationOptions,
) -> Result<SessionReflectorAutomationRun> {
    let mut run = AgentTaskRunContext::new(
        dashboard_root,
        sessions_db,
        options.run_id.clone(),
        "session_reflector",
        options.trigger,
        config,
        AgentTaskKind::SessionReflector,
    );
    if let Some(reason @ ("automation_disabled" | "session_reflector_disabled")) =
        task_skip_reason(config, AgentTaskKind::SessionReflector)
    {
        return Ok(rejected_session_reflector_run(&run, config, reason, None));
    }
    let SessionReflectorEvidenceBundle {
        evidence,
        evidence_hash,
    } = match build_session_reflector_evidence(retrieval, &options).await? {
        SessionReflectorEvidenceOutcome::Ready(bundle) => bundle,
        SessionReflectorEvidenceOutcome::Skipped {
            reason,
            evidence_hash,
        } => {
            return Ok(rejected_session_reflector_run(
                &run,
                config,
                reason,
                evidence_hash,
            ));
        }
    };
    let _run_lock = match run.gate().await? {
        SchedulerGate::Proceed(lock) => lock,
        SchedulerGate::Skip(reason) => {
            return skipped_session_reflector_run(&run, reason, evidence_hash.clone()).await;
        }
    };
    crate::automation::outcomes::refresh_fact_outcomes(
        &run.dashboard_root,
        memory,
        current_timestamp(),
    )
    .await?;

    let request = AgentTaskRequest::new(
        run.run_id.clone(),
        AgentTaskKind::SessionReflector,
        build_session_reflector_prompt(&evidence),
        evidence_hash.clone(),
        json!({
            "session_reflection_evidence": evidence,
            "apply": true,
        }),
    );
    let input_hash = Some(request.input_hash.clone());
    let finalizer = run.finalizer(input_hash.clone());
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
            return Ok(SessionReflectorAutomationRun {
                run_id: record.run_id.clone(),
                report: failed_backend_fallback_report(&record),
                ledger_record: record,
                backend_response: None,
            });
        }
    };
    let (mut proposed_ops, mut proposals) = finalizer
        .response_output_array(
            &response,
            evidence_hash.clone(),
            &retry_report,
            "facts",
            "session reflector output must include a facts array",
        )
        .await?;
    let retry_policy =
        crate::automation::backend::BackendRetryPolicy::from_timeout_secs(config.timeout_secs);
    let mut validation_repairs = Vec::new();
    loop {
        let (_, rejected_facts) =
            validate_session_fact_proposals(memory, &proposals, &evidence).await?;
        if rejected_facts.is_empty() {
            break;
        }
        let attempt = validation_repairs.len() + 1;
        validation_repairs.push(json!({
            "attempt": attempt,
            "errors": rejected_facts,
        }));
        if attempt == 2 {
            let error = TraceDecayError::Config {
                message: "session reflector validation repair budget exhausted; output quarantined"
                    .to_string(),
            };
            finalizer
                .append_failed_record(
                    response.model.clone(),
                    evidence_hash,
                    Some(proposed_ops),
                    error.to_string(),
                    &retry_report,
                )
                .await?;
            return Err(error);
        }
        let repair_request = AgentTaskRequest::new(
            run.run_id.clone(),
            AgentTaskKind::SessionReflector,
            "Repair the previous session fact JSON. Return only {\"facts\": [...]}. Preserve valid intent, fix every validation error, cite only the supplied session evidence, and do not add unrelated facts."
                .to_string(),
            evidence_hash.clone(),
            json!({
                "previous_output": proposed_ops.clone(),
                "validation_errors": validation_repairs.last(),
                "session_reflection_evidence": evidence.clone(),
                "apply": true,
            }),
        );
        let mut repair_retry_report = AgentTaskRetryReport::default();
        response = match crate::automation::backend::run_agent_task_with_retry_report(
            backend,
            &repair_request,
            &retry_policy,
            &mut repair_retry_report,
        )
        .await
        {
            Ok(response) => response,
            Err(error) => {
                retry_report.extend(&repair_retry_report);
                finalizer
                    .append_failed_record(
                        None,
                        evidence_hash,
                        Some(proposed_ops),
                        error.to_string(),
                        &retry_report,
                    )
                    .await?;
                return Err(error);
            }
        };
        retry_report.extend(&repair_retry_report);
        (proposed_ops, proposals) = finalizer
            .response_output_array(
                &response,
                evidence_hash.clone(),
                &retry_report,
                "facts",
                "session reflector repair output must include a facts array",
            )
            .await?;
    }
    let (report, record) = match finalize_session_reflector_success(
        memory,
        config,
        &finalizer,
        ProposedAgentOutput {
            response: &response,
            retry_report: &retry_report,
            evidence: &evidence,
            evidence_hash: evidence_hash.clone(),
            proposed_ops: &proposed_ops,
            proposals: &proposals,
        },
        &validation_repairs,
    )
    .await
    {
        Ok(SessionReflectorFinalization::Completed { report, record }) => (report, record),
        Ok(SessionReflectorFinalization::FailedRecorded { error, .. }) => return Err(error),
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

    Ok(SessionReflectorAutomationRun {
        run_id: run.run_id,
        report,
        ledger_record: record,
        backend_response: Some(response),
    })
}

pub async fn run_session_reflector_with_backend_and_retrieval(
    cg: &TraceDecay,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    retrieval: &dyn AutomationSessionRetrieval,
    options: SessionReflectorAutomationOptions,
) -> Result<SessionReflectorAutomationRun> {
    let sessions_db = super::project_automation_sessions(cg).await?;
    let project_memory_db = cg.open_project_store_db().await?;
    let memory = MemoryApplication::new(
        cg.project_memory_owner()?,
        DatabaseFactStore::new(&project_memory_db),
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!(
            "could not initialize project session reflector memory authority: {error}"
        ),
    })?;
    run_session_reflector_for_store(
        cg.store_layout().dashboard_root.clone(),
        sessions_db,
        retrieval,
        &memory,
        config,
        backend,
        options,
    )
    .await
}

pub async fn run_session_reflector_with_backend(
    cg: &TraceDecay,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    options: SessionReflectorAutomationOptions,
) -> Result<SessionReflectorAutomationRun> {
    let retrieval = production_project_automation_retrieval(cg).await;
    run_session_reflector_with_backend_and_retrieval(
        cg,
        config,
        backend,
        retrieval.as_ref(),
        options,
    )
    .await
}

pub(crate) async fn run_user_session_reflector_with_backend_and_retrieval(
    profile_root: &std::path::Path,
    session_registry: Arc<dyn ProfileRuntime>,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    retrieval: &dyn AutomationSessionRetrieval,
    options: SessionReflectorAutomationOptions,
) -> Result<SessionReflectorAutomationRun> {
    let sessions_db = session_registry.profile_sessions().await?;
    if let SessionReflectorEvidenceOutcome::Skipped {
        reason,
        evidence_hash,
    } = build_session_reflector_evidence(retrieval, &options).await?
    {
        let run = AgentTaskRunContext::new(
            user_automation_root(profile_root),
            Arc::clone(&sessions_db),
            options.run_id.clone(),
            "session_reflector",
            options.trigger,
            config,
            AgentTaskKind::SessionReflector,
        );
        return Ok(rejected_session_reflector_run(
            &run,
            config,
            reason,
            evidence_hash,
        ));
    }
    let memory_db = session_registry.open_user_memory_db().await?;
    let memory = MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&memory_db))
        .map_err(|error| TraceDecayError::Config {
        message: format!(
            "could not initialize profile session reflector memory authority: {error}"
        ),
    })?;
    run_session_reflector_for_store(
        user_automation_root(profile_root),
        sessions_db,
        retrieval,
        &memory,
        config,
        backend,
        options,
    )
    .await
}
