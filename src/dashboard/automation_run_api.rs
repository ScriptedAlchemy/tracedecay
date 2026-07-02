use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::Json;
use std::future::Future;
use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{json, Value};

use super::automation_run_service::{
    self, MemoryCuratorRunRequest, SessionReflectionRunRequest, SkillWritingRunRequest,
};
use super::memory_api::{
    default_agent_plan_max_clusters, default_agent_plan_min_confidence, default_dry_run,
};
use super::memory_service::{push_curation_activity, push_curation_activity_with_level};
use super::util::http_detail;
use super::DashboardState;
use crate::automation::backend::{
    agent_task_contract, classify_agent_task_error_message, prompt_version, task_key, AgentTaskKind,
};
use crate::automation::config::{effective_config, load_project_config, AutomationConfig};
use crate::automation::run_ledger::{
    append_run_record, find_run_record, load_run_records, read_run_artifact_payload,
    AutomationRunArtifact, AutomationRunArtifactKind, AutomationRunLedgerRecord,
    AutomationRunStatus, AutomationTrigger,
};
use crate::sessions::lcm::{LcmGrepSort, LcmScope};
use crate::tracedecay::current_timestamp;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MemoryCuratorRunBody {
    #[serde(default = "default_dry_run")]
    dry_run: bool,
    #[serde(default = "default_agent_plan_max_clusters")]
    max_clusters: usize,
    #[serde(default = "default_agent_plan_min_confidence")]
    min_confidence: f64,
}

impl Default for MemoryCuratorRunBody {
    fn default() -> Self {
        Self {
            dry_run: default_dry_run(),
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
pub(crate) struct SessionReflectionRunBody {
    #[serde(default = "default_dry_run")]
    dry_run: bool,
    provider: Option<String>,
    query: Option<String>,
    evidence_limit: Option<usize>,
    storage_scope: Option<String>,
    hermes_home: Option<PathBuf>,
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
            storage_scope: body.storage_scope,
            hermes_home: body.hermes_home,
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
pub(crate) struct SkillWritingRunBody {
    #[serde(default = "default_dry_run")]
    dry_run: bool,
    provider: Option<String>,
    query: Option<String>,
    evidence_limit: Option<usize>,
    storage_scope: Option<String>,
    hermes_home: Option<PathBuf>,
}

impl From<SkillWritingRunBody> for SkillWritingRunRequest {
    fn from(body: SkillWritingRunBody) -> Self {
        Self {
            provider: body.provider,
            query: body.query,
            evidence_limit: body.evidence_limit,
            storage_scope: body.storage_scope,
            hermes_home: body.hermes_home,
        }
    }
}

pub(crate) async fn memory_curator(
    State(state): State<DashboardState>,
    body: Option<axum::extract::Json<MemoryCuratorRunBody>>,
) -> (StatusCode, Json<Value>) {
    let body = body.map(|body| body.0).unwrap_or_default();
    let dry_run = body.dry_run;
    let request = MemoryCuratorRunRequest::from(body);
    run_dashboard_task_endpoint(
        state,
        dry_run,
        "memory-curator",
        AgentTaskKind::MemoryCurator,
        move |state, run_id| async move {
            Box::pin(
                automation_run_service::curation_agent_plan_payload_with_run_id(
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

pub(crate) async fn session_reflection(
    State(state): State<DashboardState>,
    body: Option<axum::extract::Json<SessionReflectionRunBody>>,
) -> (StatusCode, Json<Value>) {
    let body = body.map(|body| body.0).unwrap_or_default();
    let dry_run = body.dry_run;
    let request = SessionReflectionRunRequest::from(body);
    run_dashboard_task_endpoint(
        state,
        dry_run,
        "session-reflection",
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

pub(crate) async fn skill_writing(
    State(state): State<DashboardState>,
    body: Option<axum::extract::Json<SkillWritingRunBody>>,
) -> (StatusCode, Json<Value>) {
    let body = body.map(|body| body.0).unwrap_or_default();
    let dry_run = body.dry_run;
    let request = SkillWritingRunRequest::from(body);
    run_dashboard_task_endpoint(
        state,
        dry_run,
        "skill-writing",
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
    dry_run: bool,
    task_label: &'static str,
    task: AgentTaskKind,
    run_job: F,
) -> (StatusCode, Json<Value>)
where
    F: FnOnce(DashboardState, String) -> Fut + Send + 'static,
    Fut: Future<Output = Result<Value, String>> + Send + 'static,
{
    if !dry_run {
        return dry_run_only_response(task_label);
    }
    enqueue_dashboard_run(state, task, run_job).await
}

pub(crate) async fn artifact_list(
    State(state): State<DashboardState>,
    AxumPath(run_id): AxumPath<String>,
) -> (StatusCode, Json<Value>) {
    match find_run_record(&state.dashboard_root, &run_id).await {
        Ok(Some(record)) => {
            let count = record.artifacts.len();
            (
                StatusCode::OK,
                Json(json!({
                    "run_id": run_id,
                    "artifacts": record.artifacts,
                    "artifact_chain": artifact_chain_summary(&record.artifacts),
                    "count": count,
                    "error": "",
                })),
            )
        }
        Ok(None) => not_found(&format!("automation run '{run_id}' not found")),
        Err(err) => internal_error(&format!("Failed to load automation run artifacts: {err}")),
    }
}

pub(crate) async fn artifact_payload(
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

fn dry_run_only_response(task: &str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(http_detail(&format!(
            "{task} currently supports dry_run=true only; approval controls apply accepted drafts separately"
        ))),
    )
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

fn artifact_chain_summary(artifacts: &[AutomationRunArtifact]) -> Value {
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
        "complete": complete,
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
        eprintln!("[tracedecay] failed to mark automation run running: {err}");
    }

    match dashboard_job_skip_reason(&state, task).await {
        Ok(Some(reason)) => {
            if let Err(err) = append_skipped_record(&state, &run_id, task, reason).await {
                eprintln!("[tracedecay] failed to record automation run skip: {err}");
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

    if let Err(err) = run_job(state.clone(), run_id.clone()).await {
        append_failed_if_missing(&state, &run_id, task, err).await;
    }
}

async fn dashboard_job_skip_reason(
    state: &DashboardState,
    task: AgentTaskKind,
) -> Result<Option<&'static str>, String> {
    use crate::automation::config::{AutomationBackend, AutomationHostMode};

    let config = load_effective_dashboard_config(state).await?;
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
    };
    if !task_enabled {
        return Ok(Some(match task {
            AgentTaskKind::MemoryCurator => "memory_curator_disabled",
            AgentTaskKind::SessionReflector => "session_reflector_disabled",
            AgentTaskKind::SkillWriter => "skill_writer_disabled",
            AgentTaskKind::CombinedReview => "combined_review_disabled",
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
    let terminal_exists = load_run_records(&state.dashboard_root, 200)
        .await
        .ok()
        .into_iter()
        .flatten()
        .any(|record| record.run_id == run_id && record.status.is_terminal());
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
        eprintln!("[tracedecay] failed to record automation run failure: {err}");
    }
}

fn dashboard_task_label(task: AgentTaskKind) -> &'static str {
    match task {
        AgentTaskKind::MemoryCurator => "memory-curator",
        AgentTaskKind::SessionReflector => "session-reflector",
        AgentTaskKind::SkillWriter => "skill-writer",
        AgentTaskKind::CombinedReview => "combined-review",
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
    let config = load_effective_dashboard_config(state).await?;
    let record = dashboard_job_record(run_id, task, status, error, &config);
    append_run_record(&state.dashboard_root, &record)
        .await
        .map_err(|err| err.to_string())?;
    Ok(record)
}

async fn load_effective_dashboard_config(
    state: &DashboardState,
) -> Result<AutomationConfig, String> {
    let global = crate::user_config::UserConfig::load().automation;
    let project = load_project_config(&state.dashboard_root)
        .await
        .map_err(|err| err.to_string())?;
    effective_config(&global, project.as_ref()).map_err(|err| err.to_string())
}

fn automation_job_payload(run_id: &str, ledger_record: &AutomationRunLedgerRecord) -> Value {
    json!({
        "run_id": run_id,
        "dry_run": true,
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
        model: config.model.clone(),
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
            .map(crate::automation::backend::AgentTaskFailureClass::is_retryable),
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
