use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::Deserialize;
use serde_json::{Value, json};
use std::future::Future;

use super::DashboardState;
use super::automation_config_api::effective_automation_config;
use super::automation_run_service::{
    self, MemoryCuratorRunRequest, SessionReflectionRunRequest, SkillWritingRunRequest,
};
use super::memory_api::{default_agent_plan_max_clusters, default_agent_plan_min_confidence};
use super::memory_service::{push_curation_activity, push_curation_activity_with_level};
use super::util::http_detail;
use tracedecay_agent_hosts::automation::backend::{
    AgentTaskKind, agent_task_contract, classify_agent_task_error_message, prompt_version, task_key,
};
use tracedecay_agent_hosts::automation::config::AutomationConfig;
use tracedecay_agent_hosts::automation::run_ledger::{
    AutomationRunArtifact, AutomationRunArtifactKind, AutomationRunLedgerRecord,
    AutomationRunStatus, AutomationTrigger, append_run_record, find_run_record,
    read_published_artifact_chain, read_run_artifact_payload,
};
use tracedecay_agent_hosts::ports::session_evidence::{LcmGrepSort, LcmScope};
use tracedecay_runtime_core::tracedecay::current_timestamp;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryCuratorRunBody {
    #[serde(default = "default_agent_plan_max_clusters")]
    max_clusters: usize,
    #[serde(default = "default_agent_plan_min_confidence")]
    min_confidence: f64,
}

impl Default for MemoryCuratorRunBody {
    fn default() -> Self {
        Self {
            max_clusters: default_agent_plan_max_clusters(),
            min_confidence: default_agent_plan_min_confidence(),
        }
    }
}

impl From<MemoryCuratorRunBody> for MemoryCuratorRunRequest {
    fn from(body: MemoryCuratorRunBody) -> Self {
        Self {
            max_clusters: body.max_clusters,
            min_confidence: body.min_confidence,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionReflectionRunBody {
    provider: Option<String>,
    query: Option<String>,
    evidence_limit: Option<usize>,
    scope: Option<LcmScope>,
    session_id: Option<String>,
    include_summaries: Option<bool>,
    sort: Option<LcmGrepSort>,
    source: Option<String>,
    role: Option<String>,
    start_time: Option<i64>,
    end_time: Option<i64>,
}

impl From<SessionReflectionRunBody> for SessionReflectionRunRequest {
    fn from(body: SessionReflectionRunBody) -> Self {
        Self {
            provider: body.provider,
            query: body.query,
            evidence_limit: body.evidence_limit,
            scope: body.scope,
            session_id: body.session_id,
            include_summaries: body.include_summaries,
            sort: body.sort,
            source: body.source,
            role: body.role,
            start_time: body.start_time,
            end_time: body.end_time,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillWritingRunBody {
    provider: Option<String>,
    query: Option<String>,
    evidence_limit: Option<usize>,
}

impl From<SkillWritingRunBody> for SkillWritingRunRequest {
    fn from(body: SkillWritingRunBody) -> Self {
        Self {
            provider: body.provider,
            query: body.query,
            evidence_limit: body.evidence_limit,
        }
    }
}

pub async fn memory_curator(
    State(state): State<DashboardState>,
    body: Option<axum::extract::Json<MemoryCuratorRunBody>>,
) -> (StatusCode, Json<Value>) {
    let body = body.map(|body| body.0).unwrap_or_default();
    let request = MemoryCuratorRunRequest::from(body);
    run_dashboard_task_endpoint(
        state,
        AgentTaskKind::MemoryCurator,
        move |state, run_id| async move {
            Box::pin(
                automation_run_service::memory_curator_run_payload_with_run_id(
                    &state,
                    request,
                    Some(run_id),
                ),
            )
            .await
        },
    )
    .await
}

pub async fn session_reflection(
    State(state): State<DashboardState>,
    body: Option<axum::extract::Json<SessionReflectionRunBody>>,
) -> (StatusCode, Json<Value>) {
    let body = body.map(|body| body.0).unwrap_or_default();
    let request = SessionReflectionRunRequest::from(body);
    run_dashboard_task_endpoint(
        state,
        AgentTaskKind::SessionReflector,
        move |state, run_id| async move {
            Box::pin(
                automation_run_service::session_reflection_run_payload_with_run_id(
                    &state,
                    request,
                    Some(run_id),
                ),
            )
            .await
        },
    )
    .await
}

pub async fn skill_writing(
    State(state): State<DashboardState>,
    body: Option<axum::extract::Json<SkillWritingRunBody>>,
) -> (StatusCode, Json<Value>) {
    let body = body.map(|body| body.0).unwrap_or_default();
    let request = SkillWritingRunRequest::from(body);
    run_dashboard_task_endpoint(
        state,
        AgentTaskKind::SkillWriter,
        move |state, run_id| async move {
            Box::pin(
                automation_run_service::skill_writing_run_payload_with_run_id(
                    &state,
                    request,
                    Some(run_id),
                ),
            )
            .await
        },
    )
    .await
}

async fn run_dashboard_task_endpoint<F, Fut>(
    state: DashboardState,
    task: AgentTaskKind,
    run_job: F,
) -> (StatusCode, Json<Value>)
where
    F: FnOnce(DashboardState, String) -> Fut + Send + 'static,
    Fut: Future<Output = Result<Value, String>> + Send + 'static,
{
    enqueue_dashboard_run(state, task, run_job).await
}

#[derive(Debug, Default, Deserialize)]
pub struct RunListParams {
    limit: Option<i64>,
}

/// The newest automation runs from the ledger, projected to the fields the
/// run-history surface reads. Heavy per-run payloads (proposed/applied ops,
/// validation reports) stay behind the per-run artifact routes.
pub async fn run_list(
    State(state): State<DashboardState>,
    axum::extract::Query(params): axum::extract::Query<RunListParams>,
) -> (StatusCode, Json<Value>) {
    let limit = super::util::coerce_limit(params.limit, 50, 200) as usize;
    match tracedecay_agent_hosts::automation::run_ledger::load_run_records(
        &state.dashboard_root,
        limit,
    )
    .await
    {
        Ok(records) => {
            let runs: Vec<Value> = records.iter().map(run_history_row).collect();
            let count = runs.len();
            (
                StatusCode::OK,
                Json(json!({
                    "runs": runs,
                    "count": count,
                    "limit": limit,
                    "error": "",
                })),
            )
        }
        Err(err) => internal_error(&format!("Failed to read automation run ledger: {err}")),
    }
}

/// One ledger record as the run-history row: identity, outcome, review tallies,
/// and which artifacts exist — every field measured from the record itself.
fn run_history_row(record: &AutomationRunLedgerRecord) -> Value {
    json!({
        "run_id": record.run_id,
        "task": record.task,
        "trigger": record.trigger,
        "backend": record.backend,
        "model": record.model,
        "status": record.status,
        "reviewed_count": record.reviewed_count,
        "accepted_count": record.accepted_count,
        "rejected_count": record.rejected_count,
        "skipped_count": record.skipped_count,
        "error": record.error,
        "started_at": record.started_at,
        "completed_at": record.completed_at,
        "artifact_kinds": record
            .artifacts
            .iter()
            .map(|artifact| artifact.kind.clone())
            .collect::<Vec<_>>(),
    })
}

pub async fn artifact_list(
    State(state): State<DashboardState>,
    AxumPath(run_id): AxumPath<String>,
) -> (StatusCode, Json<Value>) {
    match find_run_record(&state.dashboard_root, &run_id).await {
        Ok(Some(record)) => {
            let count = record.artifacts.len();
            let integrity =
                read_published_artifact_chain(&state.dashboard_root, &run_id, None).await;
            let (integrity_status, integrity_verified) = match integrity {
                Ok(Some(published)) if published == record.artifacts => ("verified", true),
                Ok(Some(_)) => ("ledger_publication_mismatch", false),
                Ok(None) => ("publication_unavailable", false),
                Err(_) => ("verification_failed", false),
            };
            (
                StatusCode::OK,
                Json(json!({
                    "run_id": run_id,
                    "artifacts": record.artifacts,
                    "artifact_chain": artifact_chain_summary(
                        &record.artifacts,
                        integrity_status,
                        integrity_verified,
                    ),
                    "count": count,
                    "error": "",
                })),
            )
        }
        Ok(None) => not_found(&format!("automation run '{run_id}' not found")),
        Err(err) => internal_error(&format!("Failed to load automation run artifacts: {err}")),
    }
}

pub async fn artifact_payload(
    State(state): State<DashboardState>,
    AxumPath((run_id, kind)): AxumPath<(String, String)>,
) -> (StatusCode, Json<Value>) {
    let record = match find_run_record(&state.dashboard_root, &run_id).await {
        Ok(Some(record)) => record,
        Ok(None) => {
            return not_found(&format!("automation run '{run_id}' not found"));
        }
        Err(err) => {
            return internal_error(&format!("Failed to load automation run artifact: {err}"));
        }
    };
    let Some(artifact) = find_artifact(&record.artifacts, &kind) else {
        return not_found(&format!(
            "automation run artifact '{kind}' not found for run '{run_id}'"
        ));
    };
    match read_run_artifact_payload(&state.dashboard_root, &run_id, artifact).await {
        Ok(payload) => (
            StatusCode::OK,
            Json(json!({
                "run_id": run_id,
                "artifact": artifact,
                "payload": payload,
                "error": "",
            })),
        ),
        Err(err) => internal_error(&format!("Failed to read automation run artifact: {err}")),
    }
}

fn not_found(message: &str) -> (StatusCode, Json<Value>) {
    (StatusCode::NOT_FOUND, Json(http_detail(message)))
}

fn internal_error(message: &str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(http_detail(message)),
    )
}

fn find_artifact<'a>(
    artifacts: &'a [AutomationRunArtifact],
    kind: &str,
) -> Option<&'a AutomationRunArtifact> {
    artifacts.iter().find(|artifact| artifact.kind == kind)
}

fn artifact_chain_summary(
    artifacts: &[AutomationRunArtifact],
    integrity_status: &str,
    integrity_verified: bool,
) -> Value {
    let expected_kinds = expected_artifact_chain_kinds();
    let present_kinds = artifacts
        .iter()
        .map(|artifact| artifact.kind.as_str())
        .collect::<Vec<_>>();
    let complete = expected_kinds
        .iter()
        .all(|expected| present_kinds.iter().any(|present| present == expected));
    json!({
        "expected_kinds": expected_kinds,
        "present_kinds": present_kinds,
        "metadata_complete": complete,
        "complete": complete && integrity_verified,
        "integrity_status": integrity_status,
    })
}

fn expected_artifact_chain_kinds() -> Vec<&'static str> {
    vec![
        AutomationRunArtifactKind::Traces.as_str(),
        AutomationRunArtifactKind::Feedback.as_str(),
        AutomationRunArtifactKind::GeneratedEvals.as_str(),
        AutomationRunArtifactKind::ValidationGate.as_str(),
        AutomationRunArtifactKind::OptimizerDiagnosis.as_str(),
        AutomationRunArtifactKind::CodexHandoff.as_str(),
    ]
}

async fn enqueue_dashboard_run<F, Fut>(
    state: DashboardState,
    task: AgentTaskKind,
    run_job: F,
) -> (StatusCode, Json<Value>)
where
    F: FnOnce(DashboardState, String) -> Fut + Send + 'static,
    Fut: Future<Output = Result<Value, String>> + Send + 'static,
{
    let run_id = dashboard_run_id(task);
    let queued =
        match append_dashboard_job_record(&state, &run_id, task, AutomationRunStatus::Queued, None)
            .await
        {
            Ok(record) => record,
            Err(err) => return internal_error(&format!("Failed to queue automation run: {err}")),
        };

    match dashboard_job_skip_reason(&state, task).await {
        Ok(Some(reason)) => {
            if let Err(err) = append_immediate_skip_records(&state, &run_id, task, reason).await {
                return internal_error(&format!("Failed to queue automation run: {err}"));
            }
            push_dashboard_task_skip_activity(&state, task, reason).await;
            return (
                StatusCode::ACCEPTED,
                Json(automation_job_payload(&run_id, &queued)),
            );
        }
        Ok(None) => {}
        Err(err) => return internal_error(&format!("Failed to queue automation run: {err}")),
    }

    let payload = automation_job_payload(&run_id, &queued);
    tokio::spawn(async move {
        Box::pin(run_dashboard_job(state, run_id, task, run_job)).await;
    });
    (StatusCode::ACCEPTED, Json(payload))
}

async fn run_dashboard_job<F, Fut>(
    state: DashboardState,
    run_id: String,
    task: AgentTaskKind,
    run_job: F,
) where
    F: FnOnce(DashboardState, String) -> Fut,
    Fut: Future<Output = Result<Value, String>>,
{
    if let Err(err) = append_running_record(&state, &run_id, task).await {
        tracing::warn!(
            run_id,
            task = ?task,
            error = %err,
            "failed to mark dashboard automation run as running"
        );
    }

    match dashboard_job_skip_reason(&state, task).await {
        Ok(Some(reason)) => {
            if let Err(err) = append_skipped_record(&state, &run_id, task, reason).await {
                tracing::warn!(
                    run_id,
                    task = ?task,
                    %reason,
                    error = %err,
                    "failed to record dashboard automation run skip"
                );
            }
            push_dashboard_task_skip_activity(&state, task, reason).await;
            return;
        }
        Ok(None) => {}
        Err(err) => {
            append_failed_if_missing(&state, &run_id, task, err).await;
            return;
        }
    }

    match run_job(state.clone(), run_id.clone()).await {
        Ok(payload) => {
            if let Err(err) = append_returned_terminal_if_missing(&state, &run_id, &payload).await {
                append_failed_if_missing(&state, &run_id, task, err).await;
            }
        }
        Err(err) => append_failed_if_missing(&state, &run_id, task, err).await,
    }
}

async fn append_returned_terminal_if_missing(
    state: &DashboardState,
    run_id: &str,
    payload: &Value,
) -> Result<(), String> {
    let record = payload
        .get("ledger_record")
        .cloned()
        .ok_or_else(|| "automation run payload omitted ledger_record".to_string())
        .and_then(|record| {
            serde_json::from_value::<AutomationRunLedgerRecord>(record)
                .map_err(|err| format!("invalid automation run ledger_record: {err}"))
        })?;
    if record.run_id != run_id {
        return Err(format!(
            "automation run payload returned run_id '{}' for expected run '{run_id}'",
            record.run_id
        ));
    }
    if !record.status.is_terminal() {
        return Err(format!(
            "automation run payload returned non-terminal status '{}'",
            record.status.as_str()
        ));
    }
    let terminal_exists = find_run_record(&state.dashboard_root, run_id)
        .await
        .map_err(|err| format!("failed to inspect automation run ledger: {err}"))?
        .is_some_and(|record| record.status.is_terminal());
    if terminal_exists {
        return Ok(());
    }
    append_run_record(&state.dashboard_root, &record)
        .await
        .map_err(|err| format!("failed to record returned automation run: {err}"))
}

async fn dashboard_job_skip_reason(
    state: &DashboardState,
    task: AgentTaskKind,
) -> Result<Option<&'static str>, String> {
    use tracedecay_agent_hosts::automation::config::{AutomationBackend, AutomationHostMode};

    let config = load_effective_dashboard_config(state)?;
    if !config.enabled {
        return Ok(Some("automation_disabled"));
    }
    if config.host_mode == AutomationHostMode::DelegatedHost {
        return Ok(Some("delegated_host_mode"));
    }
    if config.backend == AutomationBackend::Disabled {
        return Ok(Some("backend_disabled"));
    }
    let task_enabled = match task {
        AgentTaskKind::MemoryCurator => config.tasks.memory_curator.enabled,
        AgentTaskKind::SessionReflector => config.tasks.session_reflector.enabled,
        AgentTaskKind::SkillWriter => config.tasks.skill_writer.enabled,
        AgentTaskKind::CombinedReview => {
            config.tasks.session_reflector.enabled && config.tasks.skill_writer.enabled
        }
        // User jobs carry their own enabled flag; the job runner gates on it.
        AgentTaskKind::UserJob => return Ok(None),
    };
    if !task_enabled {
        return Ok(Some(match task {
            AgentTaskKind::MemoryCurator => "memory_curator_disabled",
            AgentTaskKind::SessionReflector => "session_reflector_disabled",
            AgentTaskKind::SkillWriter => "skill_writer_disabled",
            AgentTaskKind::CombinedReview => "combined_review_disabled",
            AgentTaskKind::UserJob => "user_job_disabled",
        }));
    }
    Ok(None)
}

async fn append_immediate_skip_records(
    state: &DashboardState,
    run_id: &str,
    task: AgentTaskKind,
    reason: &'static str,
) -> Result<(), String> {
    append_running_record(state, run_id, task).await?;
    append_skipped_record(state, run_id, task, reason).await
}

async fn append_running_record(
    state: &DashboardState,
    run_id: &str,
    task: AgentTaskKind,
) -> Result<(), String> {
    append_dashboard_job_record(state, run_id, task, AutomationRunStatus::Running, None)
        .await
        .map(|_| ())
        .map_err(|err| format!("failed to mark automation run running: {err}"))
}

async fn append_skipped_record(
    state: &DashboardState,
    run_id: &str,
    task: AgentTaskKind,
    reason: &'static str,
) -> Result<(), String> {
    append_dashboard_job_record(
        state,
        run_id,
        task,
        AutomationRunStatus::Skipped,
        Some(reason.to_string()),
    )
    .await
    .map(|_| ())
    .map_err(|err| format!("failed to record automation run skip: {err}"))
}

async fn append_failed_if_missing(
    state: &DashboardState,
    run_id: &str,
    task: AgentTaskKind,
    err: String,
) {
    let terminal_exists = find_run_record(&state.dashboard_root, run_id)
        .await
        .ok()
        .flatten()
        .is_some_and(|record| record.status.is_terminal());
    if terminal_exists {
        return;
    }
    if task == AgentTaskKind::MemoryCurator {
        push_curation_activity_with_level(
            state,
            "failure",
            format!("Dashboard memory-curator automation run failed: {err}"),
            true,
            "error",
        )
        .await;
    }
    if let Err(err) =
        append_dashboard_job_record(state, run_id, task, AutomationRunStatus::Failed, Some(err))
            .await
    {
        tracing::warn!(
            run_id,
            task = ?task,
            error = %err,
            "failed to record dashboard automation run failure"
        );
    }
}

fn dashboard_task_label(task: AgentTaskKind) -> &'static str {
    match task {
        AgentTaskKind::MemoryCurator => "memory-curator",
        AgentTaskKind::SessionReflector => "session-reflector",
        AgentTaskKind::SkillWriter => "skill-writer",
        AgentTaskKind::CombinedReview => "combined-review",
        AgentTaskKind::UserJob => "user-job",
    }
}

async fn push_dashboard_task_skip_activity(
    state: &DashboardState,
    task: AgentTaskKind,
    reason: &str,
) {
    let task_label = dashboard_task_label(task);
    push_curation_activity(
        state,
        "queued",
        format!("Queued dashboard {task_label} automation run"),
        true,
    )
    .await;
    push_curation_activity(
        state,
        "evidence",
        format!("Skipped evidence collection for dashboard {task_label} automation run: {reason}"),
        true,
    )
    .await;
    push_curation_activity(
        state,
        "backend",
        format!("Skipped backend call for dashboard {task_label} automation run: {reason}"),
        true,
    )
    .await;
    push_curation_activity(
        state,
        "validation",
        format!("Skipped dashboard {task_label} automation run: {reason}"),
        true,
    )
    .await;
    push_curation_activity(
        state,
        "apply",
        format!("No mutations applied for dashboard {task_label} automation run: {reason}"),
        true,
    )
    .await;
    push_curation_activity(
        state,
        "report",
        format!("Dashboard {task_label} automation run skipped: {reason}"),
        true,
    )
    .await;
    push_curation_activity(
        state,
        "finish",
        format!("Finished skipped dashboard {task_label} automation run: {reason}"),
        true,
    )
    .await;
}

async fn append_dashboard_job_record(
    state: &DashboardState,
    run_id: &str,
    task: AgentTaskKind,
    status: AutomationRunStatus,
    error: Option<String>,
) -> Result<AutomationRunLedgerRecord, String> {
    let run_id = run_id.to_string();
    let payload = super::automation_run_service::execute_dashboard_automation_write(
        state,
        move |state| async move {
            let config = load_effective_dashboard_config(&state)?;
            let record = dashboard_job_record(&run_id, task, status, error, &config);
            append_run_record(&state.dashboard_root, &record)
                .await
                .map_err(|err| err.to_string())?;
            serde_json::to_value(record).map_err(|err| err.to_string())
        },
    )
    .await?;
    serde_json::from_value(payload).map_err(|err| err.to_string())
}

fn load_effective_dashboard_config(state: &DashboardState) -> Result<AutomationConfig, String> {
    effective_automation_config(state)
        .map(|(_, config)| config)
        .map_err(|error| error.to_string())
}

fn automation_job_payload(run_id: &str, ledger_record: &AutomationRunLedgerRecord) -> Value {
    json!({
        "run_id": run_id,
        "status": ledger_record.status,
        "report": {
            "status": ledger_record.status,
            "task": task_key(ledger_record.task),
            "queued": ledger_record.status == AutomationRunStatus::Queued,
        },
        "ledger_record": ledger_record,
        "backend_response": Value::Null,
    })
}

fn dashboard_job_record(
    run_id: &str,
    task: AgentTaskKind,
    status: AutomationRunStatus,
    error: Option<String>,
    config: &AutomationConfig,
) -> AutomationRunLedgerRecord {
    let now = current_timestamp().to_string();
    let fallback_status = error
        .clone()
        .filter(|_| status == AutomationRunStatus::Skipped);
    let error_classification = (status == AutomationRunStatus::Failed)
        .then(|| error.as_deref().map(classify_agent_task_error_message))
        .flatten();
    let contract = agent_task_contract(task);
    AutomationRunLedgerRecord {
        schema_version: 2,
        run_id: run_id.to_string(),
        trigger: AutomationTrigger::Dashboard,
        task,
        task_key: Some(task_key(task).to_string()),
        backend: config.backend.as_str().to_string(),
        host_mode: Some(config.host_mode.as_str().to_string()),
        prompt_version: Some(prompt_version(task).to_string()),
        response_schema: Some(contract.response_schema),
        strict_json: Some(contract.strict_json),
        model: None,
        status,
        evidence_hash: None,
        input_hash: None,
        output_hash: None,
        proposed_ops: None,
        applied_ops: None,
        rejected_ops: None,
        validation_report: None,
        reviewed_count: 0,
        accepted_count: 0,
        rejected_count: 0,
        skipped_count: usize::from(status == AutomationRunStatus::Skipped),
        error,
        error_classification,
        error_retryable: error_classification
            .map(tracedecay_agent_hosts::automation::backend::AgentTaskFailureClass::is_retryable),
        backend_attempt_count: 0,
        backend_attempts: Vec::new(),
        fallback_status,
        report_ref: Some(json!({
            "run_id": run_id,
            "task": task_key(task),
        })),
        artifacts: Vec::new(),
        started_at: now.clone(),
        completed_at: now,
    }
}

fn dashboard_run_id(task: AgentTaskKind) -> String {
    let micros = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_micros())
        .unwrap_or_default();
    format!("dashboard_{}_{}", task_key(task), micros)
}

#[cfg(test)]
mod run_list_tests {
    use super::*;

    #[test]
    fn run_history_row_projects_identity_outcome_and_artifact_kinds() {
        let record: AutomationRunLedgerRecord = serde_json::from_value(json!({
            "schema_version": 1,
            "run_id": "run-1",
            "trigger": "scheduler",
            "task": "memory_curator",
            "backend": "claude",
            "status": "succeeded",
            "reviewed_count": 4,
            "accepted_count": 3,
            "rejected_count": 1,
            "error": "quota exhausted",
            "artifacts": [{
                "schema_version": 1,
                "kind": "traces",
                "path": "runs/run-1/traces.json",
                "sha256": "ab",
                "created_at": "1754000060"
            }],
            "started_at": "1754000000",
            "completed_at": "1754000060"
        }))
        .expect("ledger record fixture parses");

        let row = run_history_row(&record);
        assert_eq!(row["run_id"], json!("run-1"));
        assert_eq!(row["task"], json!("memory_curator"));
        assert_eq!(row["status"], json!("succeeded"));
        assert_eq!(row["accepted_count"], json!(3));
        assert_eq!(row["error"], json!("quota exhausted"));
        assert_eq!(row["artifact_kinds"], json!(["traces"]));
        // The heavy per-run payloads stay behind the artifact routes: a list
        // row must never carry proposed or applied operation bodies.
        assert!(row.get("proposed_ops").is_none());
        assert!(row.get("applied_ops").is_none());
    }
}
