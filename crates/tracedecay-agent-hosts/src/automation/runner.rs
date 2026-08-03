use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::apply_policy::MemoryApplyPolicy;
use super::artifacts::sha256_json;
use super::backend::{
    AgentTaskBackend, AgentTaskKind, AgentTaskRequest, AgentTaskResponse, BackendRetryPolicy,
    extract_json_object_prefix, run_agent_task_with_retry,
};
use super::config::AutomationConfig;
use super::fact_proposals::{
    FactProposalRecord, FactProposalState, apply_fact_proposal, record_session_fact_proposals,
};
use super::lifecycle::{
    AgentRunFinalizer, AgentTaskRunContext, BackendTaskRun, SchedulerGate,
    failed_backend_fallback_report, generated_run_id, task_run_gate,
};
use super::managed_skills::list_managed_skills;
use super::run_ledger::{AutomationRunLedgerRecord, AutomationTrigger};
use super::session_reflector::validate_fact_proposals_on_connection;
use super::skill_usage::{
    DEFAULT_SKILL_OVERLAP_LIMIT, ingest_project_analytics_events, skill_overlap_candidates,
    stale_skill_recommendations, summarize_skill_usage,
};
use super::skill_writer::{
    activation_policy as skill_writer_activation_policy, skill_improvement_recommendations,
    support_file_evidence as skill_writer_support_file_evidence,
    validate_and_apply_skill_proposals,
};
use super::text::truncate_chars_for_prompt;
use crate::analytics::{ToolUsageObservation, underused_tool_family_signals};
use crate::errors::{Result, TraceDecayError};
use crate::memory::user::open_user_memory_db;
use crate::sessions::lcm::{
    LcmGrepRequest, LcmGrepSort, LcmScope, LcmSessionReplayRequest, LcmSessionReplaySlice,
};
use crate::sessions::SessionQueryDb;
use crate::tracedecay::current_timestamp;

pub use super::memory_curator::{
    MemoryCuratorAutomationOptions, MemoryCuratorAutomationRun, run_memory_curator_with_backend,
};

const SKILL_ANALYTICS_IMPORT_LIMIT: usize = 2_000;
const USER_AUTOMATION_DIR: &str = "user-automation";
const USER_SESSIONS_DB_FILENAME: &str = "user-sessions.db";

pub trait ProjectAutomationStore: Send + Sync {
    fn dashboard_root(&self) -> PathBuf;
    fn sessions_db_path(&self) -> PathBuf;
    fn project_root(&self) -> &std::path::Path;
    fn open_project_memory_db<'a>(
        &'a self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<crate::db::Database>> + Send + 'a>,
    >;
}

/// Profile-level artifact, ledger, and lock root for projectless automation.
pub fn user_automation_root(profile_root: &std::path::Path) -> PathBuf {
    profile_root.join(USER_AUTOMATION_DIR)
}

fn user_sessions_db_path(profile_root: &std::path::Path) -> PathBuf {
    profile_root.join(USER_SESSIONS_DB_FILENAME)
}

/// Bounds for the session-replay evidence channel. Worst case per session is
/// `(4 + 4) * 500 + 3 * 700 = 6_100` snippet chars, so the default three
/// sessions stay under ~5k tokens alongside the grep hits.
const SESSION_REPLAY_HEAD_TURNS: usize = 4;
const SESSION_REPLAY_TAIL_TURNS: usize = 4;
const SESSION_REPLAY_SNIPPET_CHARS: usize = 500;
const SESSION_REPLAY_SUMMARY_NODES: usize = 3;
const SESSION_REPLAY_SUMMARY_CHARS: usize = 700;
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
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    options: UserSessionAutomationOptions,
) -> Result<UserSessionAutomationRun> {
    let session_reflector = run_user_session_reflector_with_backend(
        profile_root,
        config,
        backend,
        options.session_reflector,
    )
    .await?;
    let memory_curator = crate::ports::run_user_memory_curator(
        profile_root,
        config,
        backend,
        options.memory_curator,
    )
    .await?;
    let skill_writer =
        run_user_skill_writer_with_backend(profile_root, config, backend, options.skill_writer)
            .await?;
    Ok(UserSessionAutomationRun {
        session_reflector,
        memory_curator,
        skill_writer,
    })
}

struct SkillWriterEvidenceBundle {
    profile_root: PathBuf,
    evidence: Value,
    evidence_hash: Option<String>,
}

enum SkillWriterEvidenceOutcome {
    Ready(SkillWriterEvidenceBundle),
    Skipped {
        reason: &'static str,
        evidence_hash: Option<String>,
    },
}

struct SessionReflectorEvidenceBundle {
    evidence: Value,
    evidence_hash: Option<String>,
}

enum SessionReflectorEvidenceOutcome {
    Ready(SessionReflectorEvidenceBundle),
    Skipped {
        reason: &'static str,
        evidence_hash: Option<String>,
    },
}

pub async fn run_session_reflector_with_backend(
    store: &dyn ProjectAutomationStore,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    options: SessionReflectorAutomationOptions,
) -> Result<SessionReflectorAutomationRun> {
    let memory_db = store.open_project_memory_db().await?;
    run_session_reflector_for_store(
        store.dashboard_root(),
        store.sessions_db_path(),
        memory_db.conn(),
        Some(store.project_root()),
        config,
        backend,
        options,
    )
    .await
}

/// Runs session reflection for projectless evidence and profile-level memory.
pub async fn run_user_session_reflector_with_backend(
    profile_root: &std::path::Path,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    options: SessionReflectorAutomationOptions,
) -> Result<SessionReflectorAutomationRun> {
    let memory_db = open_user_memory_db(profile_root).await?;
    run_session_reflector_for_store(
        user_automation_root(profile_root),
        user_sessions_db_path(profile_root),
        memory_db.conn(),
        None,
        config,
        backend,
        options,
    )
    .await
}

async fn run_session_reflector_for_store(
    dashboard_root: PathBuf,
    sessions_db_path: PathBuf,
    memory_conn: &libsql::Connection,
    digest_root: Option<&std::path::Path>,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    options: SessionReflectorAutomationOptions,
) -> Result<SessionReflectorAutomationRun> {
    let mut run = AgentTaskRunContext::new(
        dashboard_root,
        sessions_db_path.clone(),
        options.run_id.clone(),
        "session_reflector",
        options.trigger,
        config,
        AgentTaskKind::SessionReflector,
    );
    let _run_lock = match run.gate().await? {
        SchedulerGate::Proceed(lock) => lock,
        SchedulerGate::Skip(reason) => {
            return skipped_session_reflector_run(&run, reason, None).await;
        }
    };

    let SessionReflectorEvidenceBundle {
        evidence,
        evidence_hash,
    } = match build_session_reflector_evidence(
        &run.dashboard_root,
        &sessions_db_path,
        memory_conn,
        &options,
    )
    .await?
    {
        SessionReflectorEvidenceOutcome::Ready(bundle) => bundle,
        SessionReflectorEvidenceOutcome::Skipped {
            reason,
            evidence_hash,
        } => return skipped_session_reflector_run(&run, reason, evidence_hash).await,
    };

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
    let response = match finalizer
        .run_backend_or_fallback(backend, &request, evidence_hash.clone())
        .await?
    {
        BackendTaskRun::Response(response) => response,
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
            "facts",
            "session reflector output must include a facts array",
        )
        .await?;
    let (report, record) = finalize_session_reflector_success(
        memory_conn,
        digest_root,
        &finalizer,
        &run.dashboard_root,
        &run.run_id,
        &response,
        &evidence,
        evidence_hash,
        &proposed_ops,
        &proposals,
    )
    .await?;
    let record = finalizer
        .append_success_record(&request, &response, record)
        .await?;

    Ok(SessionReflectorAutomationRun {
        run_id: run.run_id,
        report,
        ledger_record: record,
        backend_response: Some(response),
    })
}

/// Validates and stages the `facts` half of a reflector (or combined) run,
/// returning the report plus the not-yet-appended success ledger record.
#[allow(clippy::too_many_arguments)]
async fn finalize_session_reflector_success(
    memory_conn: &libsql::Connection,
    digest_root: Option<&std::path::Path>,
    finalizer: &AgentRunFinalizer<'_>,
    dashboard_root: &std::path::Path,
    run_id: &str,
    response: &AgentTaskResponse,
    evidence: &Value,
    evidence_hash: Option<String>,
    proposed_ops: &Value,
    proposals: &[Value],
) -> Result<(Value, AutomationRunLedgerRecord)> {
    let (accepted_facts, rejected_facts) =
        validate_fact_proposals_on_connection(memory_conn, proposals, evidence).await?;
    let accepted_count = accepted_facts.len();
    let rejected_count = rejected_facts.len();
    let mut proposal_records = record_session_fact_proposals(
        dashboard_root,
        run_id,
        evidence_hash.as_deref(),
        &accepted_facts,
        &rejected_facts,
    )
    .await?;
    let auto_apply_facts = MemoryApplyPolicy::should_apply(accepted_count);
    let applied_fact_proposals = if auto_apply_facts {
        auto_apply_session_fact_proposals(
            memory_conn,
            digest_root,
            dashboard_root,
            std::mem::take(&mut proposal_records),
        )
        .await?
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
    let applied_fact_ids: Vec<i64> = applied_fact_proposals
        .iter()
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
        object.insert("applied_fact_ids".to_string(), json!(applied_fact_ids));
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

async fn auto_apply_session_fact_proposals(
    memory_conn: &libsql::Connection,
    digest_root: Option<&std::path::Path>,
    dashboard_root: &std::path::Path,
    proposal_records: Vec<FactProposalRecord>,
) -> Result<Vec<FactProposalRecord>> {
    let mut applied = Vec::with_capacity(proposal_records.len());
    for record in proposal_records {
        if record.state != FactProposalState::PendingApproval {
            applied.push(record);
            continue;
        }
        applied.push(
            apply_fact_proposal(
                dashboard_root,
                memory_conn,
                &record.proposal_id,
                Some("session_reflector:auto_apply".to_string()),
            )
            .await?,
        );
    }
    if applied
        .iter()
        .any(|record| record.state == FactProposalState::Applied)
    {
        if let Some(digest_root) = digest_root {
            crate::automation::memory_digest::refresh_memory_digest_after_memory_change(
                memory_conn,
                digest_root,
            )
            .await;
        }
    }
    Ok(applied)
}

pub async fn run_skill_writer_with_backend(
    store: &dyn ProjectAutomationStore,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    options: SkillWriterAutomationOptions,
) -> Result<SkillWriterAutomationRun> {
    run_skill_writer_for_store(
        store.dashboard_root(),
        store.sessions_db_path(),
        Some(store.project_root()),
        config,
        backend,
        options,
    )
    .await
}

/// Runs skill writing from profile-level projectless session evidence.
pub async fn run_user_skill_writer_with_backend(
    profile_root: &std::path::Path,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    mut options: SkillWriterAutomationOptions,
) -> Result<SkillWriterAutomationRun> {
    options.profile_root = Some(profile_root.to_path_buf());
    run_skill_writer_for_store(
        user_automation_root(profile_root),
        user_sessions_db_path(profile_root),
        None,
        config,
        backend,
        options,
    )
    .await
}

async fn run_skill_writer_for_store(
    dashboard_root: PathBuf,
    sessions_db_path: PathBuf,
    analytics_project_root: Option<&std::path::Path>,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    options: SkillWriterAutomationOptions,
) -> Result<SkillWriterAutomationRun> {
    let mut run = AgentTaskRunContext::new(
        dashboard_root,
        sessions_db_path.clone(),
        options.run_id.clone(),
        "skill_writer",
        options.trigger,
        config,
        AgentTaskKind::SkillWriter,
    );
    let _run_lock = match run.gate().await? {
        SchedulerGate::Proceed(lock) => lock,
        SchedulerGate::Skip(reason) => {
            return skipped_skill_writer_run(&run, reason, None).await;
        }
    };

    let evidence_bundle =
        match build_skill_writer_evidence(&sessions_db_path, analytics_project_root, options)
            .await?
        {
            SkillWriterEvidenceOutcome::Ready(bundle) => bundle,
            SkillWriterEvidenceOutcome::Skipped {
                reason,
                evidence_hash,
            } => return skipped_skill_writer_run(&run, reason, evidence_hash).await,
        };
    let SkillWriterEvidenceBundle {
        profile_root,
        evidence,
        evidence_hash,
    } = evidence_bundle;

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
        eprintln!("[tracedecay] warning: failed to refresh skill outcomes: {err}");
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
    let response = match finalizer
        .run_backend_or_fallback(backend, &request, evidence_hash.clone())
        .await?
    {
        BackendTaskRun::Response(response) => response,
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
            "skills",
            "skill writer output must include a skills array",
        )
        .await?;
    let (report, record) = finalize_skill_writer_success(
        config,
        &finalizer,
        &profile_root,
        &run.run_id,
        &response,
        &evidence,
        evidence_hash,
        activation_policy,
        &proposed_ops,
        &proposals,
    )
    .await?;
    let record = finalizer
        .append_success_record(&request, &response, record)
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
/// A skill-proposal validation failure appends a failed record before
/// bubbling the error.
#[allow(clippy::too_many_arguments)]
async fn finalize_skill_writer_success(
    config: &AutomationConfig,
    finalizer: &AgentRunFinalizer<'_>,
    profile_root: &std::path::Path,
    run_id: &str,
    response: &AgentTaskResponse,
    evidence: &Value,
    evidence_hash: Option<String>,
    activation_policy: &'static str,
    proposed_ops: &Value,
    proposals: &[Value],
) -> Result<(Value, AutomationRunLedgerRecord)> {
    let proposal_outcome = match validate_and_apply_skill_proposals(
        profile_root,
        run_id,
        proposals,
        config.auto_enable_skills,
    )
    .await
    {
        Ok(result) => result,
        Err(err) => {
            finalizer
                .append_failed_record(
                    response.model.clone(),
                    evidence_hash,
                    Some(proposed_ops.clone()),
                    err.to_string(),
                )
                .await?;
            return Err(err);
        }
    };
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

async fn build_session_reflector_evidence(
    dashboard_root: &std::path::Path,
    sessions_db_path: &std::path::Path,
    memory_conn: &libsql::Connection,
    options: &SessionReflectorAutomationOptions,
) -> Result<SessionReflectorEvidenceOutcome> {
    // Refresh outcomes of previously applied fact proposals so this run's
    // feedback artifact reports real post-apply quality. Best effort: a
    // missing memory store must not block reflection.
    if let Err(err) =
        super::outcomes::refresh_fact_outcomes(dashboard_root, memory_conn, current_timestamp())
            .await
    {
        eprintln!("[tracedecay] warning: failed to refresh fact outcomes: {err}");
    }

    let provider = normalized_non_empty(&options.provider).unwrap_or_else(default_session_provider);
    let query =
        normalized_non_empty(&options.query).unwrap_or_else(default_session_reflection_query);
    let evidence_limit = options.evidence_limit.clamp(1, 50);
    let session_id = options.session_id.as_deref().and_then(normalized_non_empty);
    let source = options.source.as_deref().and_then(normalized_non_empty);
    let role = options.role.as_deref().and_then(normalized_non_empty);

    if !sessions_db_path.is_file() {
        return Ok(SessionReflectorEvidenceOutcome::Skipped {
            reason: "lcm_not_ingested",
            evidence_hash: None,
        });
    }
    let Some(lcm_db) = SessionQueryDb::open_read_only_at(sessions_db_path).await else {
        return Ok(SessionReflectorEvidenceOutcome::Skipped {
            reason: "lcm_unavailable",
            evidence_hash: None,
        });
    };
    let hits = lcm_db
        .lcm_grep(LcmGrepRequest {
            provider: provider.clone(),
            query: query.clone(),
            scope: options.scope,
            session_id: session_id.clone(),
            include_summaries: options.include_summaries,
            limit: evidence_limit,
            sort: options.sort,
            source: source.clone(),
            role: role.clone(),
            start_time: options.start_time,
            end_time: options.end_time,
            git_filter: crate::sessions::git_correlation::GitScopeFilter::default(),
        })
        .await
        .map_err(|e| TraceDecayError::Config {
            message: format!("failed to build session reflection evidence: {e}"),
        })?
        .hits;
    let recent_session_slices = if options.include_recent_sessions
        && session_reflector_replay_allowed(
            options.scope,
            session_id.as_deref(),
            source.as_deref(),
            role.as_deref(),
            options.start_time,
            options.end_time,
        ) {
        recent_session_replay_evidence(
            &lcm_db,
            &provider,
            session_id.as_deref(),
            options.include_summaries,
            options.recent_sessions_limit,
            "session_reflector",
        )
        .await?
    } else {
        None
    };
    let evidence = json!({
        "evidence_mode": evidence_mode_label(recent_session_slices.is_some()),
        "provider": provider,
        "query": query,
        "scope": options.scope,
        "session_id": session_id,
        "include_summaries": options.include_summaries,
        "sort": options.sort,
        "source": source,
        "role": role,
        "start_time": options.start_time,
        "end_time": options.end_time,
        "recent_session_slices": recent_session_slices,
        "hits": hits,
    });
    let evidence_hash = Some(sha256_json(&evidence));
    let has_grep_hits = evidence
        .get("hits")
        .and_then(Value::as_array)
        .is_some_and(|hits| !hits.is_empty());
    let has_replay_sessions = evidence
        .pointer("/recent_session_slices/sessions")
        .and_then(Value::as_array)
        .is_some_and(|sessions| !sessions.is_empty());
    if !has_grep_hits && !has_replay_sessions {
        return Ok(SessionReflectorEvidenceOutcome::Skipped {
            reason: "no_session_evidence",
            evidence_hash,
        });
    }

    Ok(SessionReflectorEvidenceOutcome::Ready(
        SessionReflectorEvidenceBundle {
            evidence,
            evidence_hash,
        },
    ))
}

async fn build_skill_writer_evidence(
    sessions_db_path: &std::path::Path,
    analytics_project_root: Option<&std::path::Path>,
    options: SkillWriterAutomationOptions,
) -> Result<SkillWriterEvidenceOutcome> {
    let profile_root = match options.profile_root {
        Some(path) => path,
        None => crate::storage::default_profile_root()?,
    };
    let provider =
        normalized_non_empty(&options.provider).unwrap_or_else(default_skill_writer_provider);
    let query = normalized_non_empty(&options.query).unwrap_or_else(default_skill_writer_query);
    let evidence_limit = options.evidence_limit.clamp(1, 50);

    if !sessions_db_path.is_file() {
        return Ok(SkillWriterEvidenceOutcome::Skipped {
            reason: "lcm_not_ingested",
            evidence_hash: None,
        });
    }
    let Some(lcm_db) = SessionQueryDb::open_read_only_at(sessions_db_path).await else {
        return Ok(SkillWriterEvidenceOutcome::Skipped {
            reason: "lcm_unavailable",
            evidence_hash: None,
        });
    };
    let hits = lcm_db
        .lcm_grep(LcmGrepRequest {
            provider: provider.clone(),
            query: query.clone(),
            scope: LcmScope::All,
            session_id: None,
            include_summaries: true,
            limit: evidence_limit,
            sort: LcmGrepSort::Recency,
            source: None,
            role: None,
            start_time: None,
            end_time: None,
            git_filter: crate::sessions::git_correlation::GitScopeFilter::default(),
        })
        .await
        .map_err(|e| TraceDecayError::Config {
            message: format!("failed to build skill writer evidence: {e}"),
        })?
        .hits;
    let recent_session_slices = if options.include_recent_sessions {
        recent_session_replay_evidence(
            &lcm_db,
            &provider,
            None,
            true,
            options.recent_sessions_limit,
            "skill_writer",
        )
        .await?
    } else {
        None
    };
    let existing_skills = list_managed_skills(&profile_root).await?;
    if let Some(project_root) = analytics_project_root {
        ingest_project_analytics_events(
            &profile_root,
            project_root,
            SKILL_ANALYTICS_IMPORT_LIMIT,
        )
        .await?;
    }
    let skill_usage_summaries = summarize_skill_usage(&profile_root, &existing_skills).await?;
    let stale_recommendations = stale_skill_recommendations(
        &skill_usage_summaries,
        current_timestamp(),
        60 * 60 * 24 * 90,
    );
    let underused_tool_families = lcm_db
        .session_tool_usage_rows(10_000)
        .await
        .map(|rows| {
            underused_tool_family_signals(rows.iter().map(|row| ToolUsageObservation {
                tool_names: Some(row.tool_names.as_str()),
                metadata_json: Some(row.metadata_json.as_str()),
                text: Some(row.text.as_str()),
            }))
        })
        .unwrap_or_default();
    let overlap_candidates =
        skill_overlap_candidates(&existing_skills, DEFAULT_SKILL_OVERLAP_LIMIT);
    let skill_improvement_recommendations = skill_improvement_recommendations(
        &hits,
        &skill_usage_summaries,
        &stale_recommendations,
        &underused_tool_families,
        &overlap_candidates,
    );
    let evidence = json!({
        "evidence_mode": evidence_mode_label(recent_session_slices.is_some()),
        "provider": provider,
        "query": query,
        "recent_session_slices": recent_session_slices,
        "hits": hits,
        "skill_usage_summaries": skill_usage_summaries,
        "stale_recommendations": stale_recommendations,
        "underused_tool_families": underused_tool_families,
        "skill_overlap_candidates": overlap_candidates,
        "skill_improvement_recommendations": skill_improvement_recommendations,
        "existing_managed_skills": existing_skills
            .iter()
            .map(|skill| json!({
                "id": skill.metadata.id,
                "title": skill.metadata.title,
                "summary": skill.metadata.summary,
                "category": skill.metadata.category,
                "state": skill.metadata.state,
                "pinned": skill.metadata.pinned,
                "checksum": skill.metadata.checksum,
                "updated_at": skill.metadata.updated_at,
                "body_markdown": truncate_chars_for_prompt(&skill.body_markdown, 4000),
                "support_files": skill.support_files
                    .iter()
                    .map(skill_writer_support_file_evidence)
                    .collect::<Vec<_>>(),
            }))
            .collect::<Vec<_>>(),
    });
    let evidence_hash = Some(sha256_json(&evidence));
    let has_grep_hits = evidence
        .get("hits")
        .and_then(Value::as_array)
        .is_some_and(|hits| !hits.is_empty());
    let has_replay_sessions = evidence
        .pointer("/recent_session_slices/sessions")
        .and_then(Value::as_array)
        .is_some_and(|sessions| !sessions.is_empty());
    if !has_grep_hits && !has_replay_sessions {
        return Ok(SkillWriterEvidenceOutcome::Skipped {
            reason: "no_skill_writer_evidence",
            evidence_hash,
        });
    }

    Ok(SkillWriterEvidenceOutcome::Ready(
        SkillWriterEvidenceBundle {
            profile_root,
            evidence,
            evidence_hash,
        },
    ))
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

async fn skipped_skill_writer_run(
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
    store: &dyn ProjectAutomationStore,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    options: CombinedReviewAutomationOptions,
) -> Result<CombinedReviewDispatch> {
    if !config.combine_due_tasks {
        return Ok(CombinedReviewDispatch::NotCombined {
            reason: "combined_mode_disabled",
        });
    }
    let dashboard_root = store.dashboard_root();
    let sessions_db_path = store.sessions_db_path();
    let memory_db = store.open_project_memory_db().await?;
    let started_at = current_timestamp().to_string();

    let (reflector_gate, _) = task_run_gate(
        config,
        &dashboard_root,
        &sessions_db_path,
        AgentTaskKind::SessionReflector,
        options.trigger,
    )
    .await?;
    let _reflector_lock = match reflector_gate {
        SchedulerGate::Proceed(lock) => lock,
        SchedulerGate::Skip(_) => {
            return Ok(CombinedReviewDispatch::NotCombined {
                reason: "session_reflector_not_due",
            });
        }
    };
    let (skill_gate, _) = task_run_gate(
        config,
        &dashboard_root,
        &sessions_db_path,
        AgentTaskKind::SkillWriter,
        options.trigger,
    )
    .await?;
    let _skill_lock = match skill_gate {
        SchedulerGate::Proceed(lock) => lock,
        SchedulerGate::Skip(_) => {
            return Ok(CombinedReviewDispatch::NotCombined {
                reason: "skill_writer_not_due",
            });
        }
    };

    let reflector_bundle = match build_session_reflector_evidence(
        &dashboard_root,
        &sessions_db_path,
        memory_db.conn(),
        &options.session_reflector,
    )
    .await?
    {
        SessionReflectorEvidenceOutcome::Ready(bundle) => bundle,
        SessionReflectorEvidenceOutcome::Skipped { .. } => {
            return Ok(CombinedReviewDispatch::NotCombined {
                reason: "session_reflector_evidence_unavailable",
            });
        }
    };
    let skill_bundle = match build_skill_writer_evidence(
        &sessions_db_path,
        Some(store.project_root()),
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

    let run_id = options
        .run_id
        .unwrap_or_else(|| generated_run_id("combined_review"));
    let reflector_run_id = format!("{run_id}_facts");
    let skill_run_id = format!("{run_id}_skills");
    let activation_policy = skill_writer_activation_policy(config);
    let combined_evidence_hash = Some(sha256_json(&json!({
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
    let response = match run_agent_task_with_retry(backend, &request, &retry_policy).await {
        Ok(response) => response,
        Err(err) => {
            let reflector_record = reflector_finalizer
                .append_backend_fallback_record(
                    reflector_bundle.evidence_hash.clone(),
                    err.to_string(),
                )
                .await?;
            let skill_record = skill_finalizer
                .append_backend_fallback_record(skill_bundle.evidence_hash.clone(), err.to_string())
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
            let err: TraceDecayError = err.into();
            let (reflector_record, skill_record) = append_combined_failed_records(
                &reflector_finalizer,
                &skill_finalizer,
                &response,
                &reflector_bundle,
                &skill_bundle,
                None,
                &err,
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
            &reflector_bundle,
            &skill_bundle,
            Some(&output),
            &err,
        )
        .await?;
        return Ok(CombinedReviewDispatch::RecordedFailure {
            run: combined_failed_run(run_id, reflector_record, skill_record, &response),
            error: err,
        });
    };

    let (reflector_report, reflector_record) = finalize_session_reflector_success(
        memory_db.conn(),
        Some(store.project_root()),
        &reflector_finalizer,
        &dashboard_root,
        &reflector_run_id,
        &response,
        &reflector_bundle.evidence,
        reflector_bundle.evidence_hash.clone(),
        &output,
        &facts,
    )
    .await?;
    let reflector_record = reflector_finalizer
        .append_success_record(&request, &response, reflector_record)
        .await?;

    let (skill_report, skill_record) = finalize_skill_writer_success(
        config,
        &skill_finalizer,
        &skill_bundle.profile_root,
        &skill_run_id,
        &response,
        &skill_bundle.evidence,
        skill_bundle.evidence_hash.clone(),
        activation_policy,
        &output,
        &skills,
    )
    .await?;
    let skill_record = skill_finalizer
        .append_success_record(&request, &response, skill_record)
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
    reflector_bundle: &SessionReflectorEvidenceBundle,
    skill_bundle: &SkillWriterEvidenceBundle,
    proposed_ops: Option<&Value>,
    err: &TraceDecayError,
) -> Result<(AutomationRunLedgerRecord, AutomationRunLedgerRecord)> {
    let reflector_record = reflector_finalizer
        .append_failed_record(
            response.model.clone(),
            reflector_bundle.evidence_hash.clone(),
            proposed_ops.cloned(),
            err.to_string(),
        )
        .await?;
    let skill_record = skill_finalizer
        .append_failed_record(
            response.model.clone(),
            skill_bundle.evidence_hash.clone(),
            proposed_ops.cloned(),
            err.to_string(),
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

fn build_session_reflector_prompt(evidence: &Value) -> String {
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

fn build_skill_writer_prompt(evidence: &Value) -> String {
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
        "Response contract: Return only JSON with a skills array of managed skill creates or updates. New skills may omit action or use action=create and must include id, title, summary, category, body_markdown, optional targets, optional support_files with text content, and reason. Targets, when present, must be an array using cursor, codex, claude, agents, opencode, kimi, kiro, or hermes; Hermes exports are generated read-only under the TraceDecay plugin package and never overwrite host-owned user skills. Updates must use action=update or action=patch, include id and base_checksum, and include at least one changed field among title, summary, category, targets, body_markdown/body, support_files, or pinned. For updates, support_files is a complete replacement list, not a partial file patch. Consolidations: when skill_overlap_candidates shows overlapping managed skills, you may propose action=merge (include id for the surviving skill, base_checksum, source_skill_id, source_base_checksum, reason, and optional merged title/summary/category/targets/body_markdown/support_files) or action=archive (include id, base_checksum, reason). Consolidations preserve archived source content. The runner stages them by default and may auto-apply only when auto_enable_skills is explicitly enabled and every ownership, checksum, pin, pending-update, and scheduled-job guard passes. Never propose merge or archive for pinned or user-authored skills. Activation is controlled only by the runner policy; do not assume activation from your response.\n",
    );
    format!(
        "{POLICY}{}",
        serde_json::to_string_pretty(evidence).unwrap_or_else(|_| "{}".to_string())
    )
}

fn normalized_non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

struct ReplaySessionTarget {
    provider: String,
    session_id: String,
}

/// Builds the "recent completed sessions" replay evidence channel: bounded
/// turn-ordered head/tail slices plus top summary-DAG nodes for the last N
/// recently active sessions (or one explicitly requested session).
///
/// Returns `None` when no session has any raw messages, so callers can fall
/// back to grep-only evidence.
async fn recent_session_replay_evidence(
    lcm_db: &SessionQueryDb,
    provider: &str,
    explicit_session_id: Option<&str>,
    include_summaries: bool,
    sessions_limit: usize,
    task_name: &str,
) -> Result<Option<Value>> {
    let sessions_limit = sessions_limit.clamp(1, 10);
    let provider_filter = (provider != "all").then_some(provider);
    let replay_error = |e: crate::sessions::lcm::LcmError| TraceDecayError::Config {
        message: format!("failed to build {task_name} session replay evidence: {e}"),
    };
    let (session_selection, targets) = if let Some(session_id) = explicit_session_id {
        let providers = match provider_filter {
            Some(provider) => vec![provider.to_string()],
            None => lcm_db
                .lcm_session_providers(session_id)
                .await
                .map_err(replay_error)?,
        };
        let targets: Vec<ReplaySessionTarget> = providers
            .into_iter()
            .map(|provider| ReplaySessionTarget {
                provider,
                session_id: session_id.to_string(),
            })
            .collect();
        ("explicit_session_id", targets)
    } else {
        let targets: Vec<ReplaySessionTarget> = lcm_db
            .lcm_recent_sessions(provider_filter, sessions_limit)
            .await
            .map_err(replay_error)?
            .into_iter()
            .map(|session| ReplaySessionTarget {
                provider: session.provider,
                session_id: session.session_id,
            })
            .collect();
        ("recent_activity", targets)
    };

    let mut sessions: Vec<LcmSessionReplaySlice> = Vec::new();
    for target in targets {
        let slice = lcm_db
            .lcm_session_replay_slice(&LcmSessionReplayRequest {
                provider: target.provider,
                session_id: target.session_id,
                head_limit: SESSION_REPLAY_HEAD_TURNS,
                tail_limit: SESSION_REPLAY_TAIL_TURNS,
                max_snippet_chars: SESSION_REPLAY_SNIPPET_CHARS,
                summary_limit: if include_summaries {
                    SESSION_REPLAY_SUMMARY_NODES
                } else {
                    0
                },
                max_summary_chars: SESSION_REPLAY_SUMMARY_CHARS,
            })
            .await
            .map_err(replay_error)?;
        if slice.total_messages == 0 && slice.summary_nodes.is_empty() {
            continue;
        }
        sessions.push(slice);
    }
    if sessions.is_empty() {
        return Ok(None);
    }
    Ok(Some(json!({
        "mode": "recent_sessions",
        "session_selection": session_selection,
        "sessions_limit": sessions_limit,
        "bounds": {
            "head_turns": SESSION_REPLAY_HEAD_TURNS,
            "tail_turns": SESSION_REPLAY_TAIL_TURNS,
            "snippet_chars": SESSION_REPLAY_SNIPPET_CHARS,
            "summary_nodes": if include_summaries {
                SESSION_REPLAY_SUMMARY_NODES
            } else {
                0
            },
            "summary_chars": SESSION_REPLAY_SUMMARY_CHARS,
        },
        "sessions": sessions,
    })))
}

/// Names the evidence channels actually present so run artifacts can
/// distinguish replay-backed runs from grep-only runs.
fn evidence_mode_label(has_replay: bool) -> &'static str {
    if has_replay {
        "session_replay_with_grep"
    } else {
        "grep_only"
    }
}

fn session_reflector_replay_allowed(
    scope: LcmScope,
    session_id: Option<&str>,
    source: Option<&str>,
    role: Option<&str>,
    start_time: Option<i64>,
    end_time: Option<i64>,
) -> bool {
    if source.is_some() || role.is_some() || start_time.is_some() || end_time.is_some() {
        return false;
    }

    matches!(scope, LcmScope::All) || session_id.is_some()
}

fn default_session_provider() -> String {
    "cursor".to_string()
}

fn default_skill_writer_provider() -> String {
    "all".to_string()
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

fn default_session_reflection_query() -> String {
    "remember prefer decision requirement workflow".to_string()
}

fn default_session_evidence_limit() -> usize {
    20
}

fn default_include_recent_sessions() -> bool {
    true
}

fn default_recent_sessions_limit() -> usize {
    3
}

fn default_skill_writer_query() -> String {
    "workflow correction repeated skill tool pattern".to_string()
}

fn default_skill_writer_evidence_limit() -> usize {
    20
}
