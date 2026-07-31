use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use std::path::PathBuf;
use std::sync::Arc;

use tracedecay_domain::FactOwnerV1;
use tracedecay_store::FactCompatibilityStore;

use super::user_automation_root;
use crate::application::memory::MemoryApplication;
use crate::automation::apply_policy::MemoryApplyPolicy;
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
use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;
use crate::memory::user::open_user_memory_db;
use crate::sessions::lcm::{LcmGrepSort, LcmScope};
use crate::store::memory::DatabaseFactStore;
use crate::tracedecay::{TraceDecay, current_timestamp};

use super::evidence::{
    SessionReflectorEvidenceBundle, SessionReflectorEvidenceOutcome,
    build_session_reflector_evidence,
};
use super::retrieval::{AutomationSessionRetrieval, production_project_automation_retrieval};
use super::unpersisted_rejected_parts;

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

pub(super) async fn validate_session_fact_proposals<A: FactCompatibilityStore>(
    memory: &MemoryApplication<A>,
    proposals: &[Value],
    evidence: &Value,
) -> Result<(Vec<Value>, Vec<Value>)> {
    validate_fact_proposals(memory, proposals, evidence).await
}

pub(super) async fn auto_apply_session_fact_proposals<A: FactCompatibilityStore>(
    memory: &MemoryApplication<A>,
    digest_root: Option<&std::path::Path>,
    dashboard_root: &std::path::Path,
    proposal_records: Vec<FactProposalRecord>,
) -> Result<(Vec<FactProposalRecord>, bool)> {
    let mut applied = Vec::with_capacity(proposal_records.len());
    let mut newly_promoted = false;
    for record in proposal_records {
        if record.state != FactProposalState::PendingApproval {
            applied.push(record);
            continue;
        }
        let result = match apply_fact_proposal_with_result(
            memory,
            dashboard_root,
            &record.proposal_id,
            Some("session_reflector:auto_apply".to_string()),
        )
        .await
        {
            Ok(result) => result,
            Err(error) => {
                refresh_auto_apply_digest_for_new_promotions(memory, digest_root, newly_promoted)
                    .await;
                return Err(error);
            }
        };
        newly_promoted |= result.newly_promoted;
        applied.push(result.record);
    }
    refresh_auto_apply_digest_for_new_promotions(memory, digest_root, newly_promoted).await;
    Ok((applied, newly_promoted))
}

async fn refresh_auto_apply_digest_for_new_promotions<A: FactCompatibilityStore>(
    memory: &MemoryApplication<A>,
    digest_root: Option<&std::path::Path>,
    newly_promoted: bool,
) {
    if !newly_promoted {
        return;
    }
    if let Some(digest_root) = digest_root {
        crate::automation::memory_digest::refresh_memory_digest_after_memory_change(
            memory,
            digest_root,
        )
        .await;
    }
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

/// Validates and stages the `facts` half of a reflector (or combined) run,
/// returning the report plus the not-yet-appended success ledger record.
pub(super) struct ProposedAgentOutput<'a> {
    pub(super) response: &'a AgentTaskResponse,
    pub(super) retry_report: &'a AgentTaskRetryReport,
    pub(super) evidence: &'a Value,
    pub(super) evidence_hash: Option<String>,
    pub(super) proposed_ops: &'a Value,
    pub(super) proposals: &'a [Value],
}

pub(super) async fn finalize_session_reflector_success<A: FactCompatibilityStore>(
    memory: &MemoryApplication<A>,
    digest_root: Option<&std::path::Path>,
    finalizer: &AgentRunFinalizer<'_>,
    output: ProposedAgentOutput<'_>,
) -> Result<(Value, AutomationRunLedgerRecord)> {
    let ProposedAgentOutput {
        response,
        retry_report,
        evidence,
        evidence_hash,
        proposed_ops,
        proposals,
    } = output;
    let dashboard_root = finalizer.dashboard_root();
    let run_id = finalizer.run_id();
    let (accepted_facts, rejected_facts) =
        validate_session_fact_proposals(memory, proposals, evidence).await?;
    let accepted_count = accepted_facts.len();
    let rejected_count = rejected_facts.len();
    let mut proposal_records = record_session_fact_proposals(
        memory,
        dashboard_root,
        run_id,
        evidence_hash.as_deref(),
        &accepted_facts,
        &rejected_facts,
    )
    .await?;
    let auto_apply_facts = MemoryApplyPolicy::should_apply(accepted_count);
    let applied_fact_proposals = if auto_apply_facts {
        let (records, _) = auto_apply_session_fact_proposals(
            memory,
            digest_root,
            dashboard_root,
            std::mem::take(&mut proposal_records),
        )
        .await?;
        records
    } else {
        Vec::new()
    };
    if auto_apply_facts {
        proposal_records.clone_from(&applied_fact_proposals);
    }
    let proposal_ids: Vec<String> = proposal_records
        .iter()
        .map(|record| record.proposal_id.clone())
        .collect();
    let applied_proposal_ids: Vec<String> = applied_fact_proposals
        .iter()
        .filter(|record| record.state == FactProposalState::Applied)
        .map(|record| record.proposal_id.clone())
        .collect();
    let applied_canonical_fact_ids: Vec<String> = applied_fact_proposals
        .iter()
        .filter(|record| record.state == FactProposalState::Applied)
        .filter_map(|record| record.applied_canonical_fact_id.clone())
        .collect();
    let applied_legacy_fact_ids: Vec<i64> = applied_fact_proposals
        .iter()
        .filter(|record| record.state == FactProposalState::Applied)
        .filter_map(|record| record.applied_fact_id)
        .collect();
    let applied_count = applied_proposal_ids.len();
    let fully_applied = accepted_count > 0 && applied_count == accepted_count;
    let mut session_fact_apply_policy =
        MemoryApplyPolicy::session_facts(accepted_count, applied_count, auto_apply_facts).to_json();
    if let Some(object) = session_fact_apply_policy.as_object_mut() {
        object.insert(
            "applied_proposal_ids".to_string(),
            json!(applied_proposal_ids),
        );
        object.insert(
            "applied_canonical_fact_ids".to_string(),
            json!(applied_canonical_fact_ids),
        );
        // Compatibility-only numeric mappings. Canonical IDs above are the
        // primary fact identities reported by session reflection.
        object.insert(
            "applied_legacy_fact_ids".to_string(),
            json!(applied_legacy_fact_ids),
        );
        object.insert(
            "applied_fact_ids".to_string(),
            json!(applied_legacy_fact_ids),
        );
        object.insert("applied_count".to_string(), json!(applied_count));
        object.insert("fully_applied".to_string(), json!(fully_applied));
    }
    let report = json!({
        "status": if auto_apply_facts { "auto_applied" } else { "needs_approval" },
        "dry_run": !auto_apply_facts,
        "task": "session_reflector",
        "evidence_hash": evidence_hash,
        "accepted_facts": accepted_facts,
        "rejected_facts": rejected_facts,
        "proposal_ids": proposal_ids,
        "proposal_records": proposal_records,
        "session_fact_apply_policy": session_fact_apply_policy,
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
        .pointer("/session_fact_apply_policy/applied_proposal_ids")
        .filter(|value| value.as_array().is_some_and(|items| !items.is_empty()))
        .cloned();
    record.rejected_ops = report.get("rejected_facts").cloned();
    let proposal_review_key = if auto_apply_facts {
        "applied_proposals"
    } else {
        "pending_proposals"
    };
    let proposal_review_ids = if auto_apply_facts {
        report
            .pointer("/session_fact_apply_policy/applied_proposal_ids")
            .cloned()
    } else {
        report.get("proposal_ids").cloned()
    }
    .unwrap_or_else(|| json!([]));
    let mut validation_report = json!({
        "status": report.get("status").cloned().unwrap_or_else(|| json!("needs_approval")),
        "dry_run": report.get("dry_run").cloned().unwrap_or(json!(true)),
        "accepted_count": accepted_count,
        "rejected_count": rejected_count,
        "session_fact_apply_policy": report.get("session_fact_apply_policy").cloned().unwrap_or_else(|| json!({})),
    });
    if let Some(object) = validation_report.as_object_mut() {
        object.insert(
            proposal_review_key.to_string(),
            json!({
            "proposal_ids": proposal_review_ids,
            "accepted_facts": report.get("accepted_facts").cloned().unwrap_or_else(|| json!([])),
            }),
        );
    }
    record.validation_report = Some(validation_report);
    Ok((report, record))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_session_reflector_for_store<A: FactCompatibilityStore>(
    dashboard_root: PathBuf,
    sessions_db: Arc<RegisteredGlobalDb>,
    retrieval: &dyn AutomationSessionRetrieval,
    memory: &MemoryApplication<A>,
    digest_root: Option<&std::path::Path>,
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
    if let Err(err) = crate::automation::outcomes::refresh_fact_outcomes(
        &run.dashboard_root,
        memory,
        current_timestamp(),
    )
    .await
    {
        tracing::warn!(error = %err, "failed to refresh fact outcomes");
    }

    let request = AgentTaskRequest::new(
        run.run_id.clone(),
        AgentTaskKind::SessionReflector,
        build_session_reflector_prompt(&evidence),
        evidence_hash.clone(),
        json!({
            "session_reflection_evidence": evidence,
            "apply": false,
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
            return Ok(SessionReflectorAutomationRun {
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
            "facts",
            "session reflector output must include a facts array",
        )
        .await?;
    let (report, record) = match finalize_session_reflector_success(
        memory,
        digest_root,
        &finalizer,
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
    let memory =
        MemoryApplication::new(cg.project_memory_owner()?, DatabaseFactStore::new(cg.db()))
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
        Some(cg.store_layout().project_root.as_path()),
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
    session_registry: Arc<DaemonSessionRuntimeRegistryV1>,
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
    let memory_db = open_user_memory_db(session_registry.as_ref()).await?;
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
        None,
        config,
        backend,
        options,
    )
    .await
}
