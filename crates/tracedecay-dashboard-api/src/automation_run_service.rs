use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::{Value, json};

use super::memory_service::{push_curation_activity, push_curation_activity_with_level};
use super::{DashboardAutomationTask, DashboardState};
use crate::sessions::lcm::{LcmGrepSort, LcmScope};

pub type DashboardAutomationWriteFuture =
    Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'static>>;
pub type DashboardAutomationWriteOperation =
    Box<dyn FnOnce() -> DashboardAutomationWriteFuture + Send + 'static>;
pub type DashboardAutomationWriter = Arc<
    dyn Fn(DashboardAutomationWriteOperation) -> DashboardAutomationWriteFuture
        + Send
        + Sync
        + 'static,
>;

pub fn execute_dashboard_automation_run_direct(
    operation: DashboardAutomationWriteOperation,
) -> DashboardAutomationWriteFuture {
    operation()
}

pub fn direct_dashboard_automation_writer() -> DashboardAutomationWriter {
    Arc::new(execute_dashboard_automation_run_direct)
}

async fn execute_dashboard_automation_run<Operation, OperationFuture>(
    state: &DashboardState,
    operation: Operation,
) -> Result<Value, String>
where
    Operation: FnOnce(DashboardState) -> OperationFuture + Send + 'static,
    OperationFuture: Future<Output = Result<Value, String>> + Send + 'static,
{
    let writer = Arc::clone(&state.automation_writer);
    let state = state.clone();
    writer(Box::new(move || Box::pin(operation(state)))).await
}

pub struct MemoryCuratorRunRequest {
    pub max_clusters: usize,
    pub min_confidence: f64,
}

pub async fn memory_curator_run_payload_with_run_id(
    state: &DashboardState,
    request: MemoryCuratorRunRequest,
    run_id: Option<String>,
) -> Result<Value, String> {
    execute_dashboard_automation_run(state, move |state| async move {
        memory_curator_run_payload_with_run_id_direct(&state, request, run_id).await
    })
    .await
}

async fn memory_curator_run_payload_with_run_id_direct(
    state: &DashboardState,
    request: MemoryCuratorRunRequest,
    run_id: Option<String>,
) -> Result<Value, String> {
    push_curation_activity(
        state,
        "queued",
        "Queued standalone memory-curator automation run",
        true,
    )
    .await;
    push_curation_activity(
        state,
        "evidence",
        format!(
            "Collecting memory-curator evidence with up to {} cluster(s) at confidence floor {:.2}",
            request.max_clusters, request.min_confidence
        ),
        true,
    )
    .await;

    push_curation_activity(
        state,
        "backend",
        "Running standalone memory-curator backend review",
        true,
    )
    .await;
    let payload = match execute_automation_task(
        state,
        DashboardAutomationTask::MemoryCurator {
            max_clusters: request.max_clusters,
            min_confidence: request.min_confidence,
            run_id,
        },
    )
    .await
    {
        Ok(payload) => payload,
        Err(err) => {
            push_curation_activity_with_level(
                state,
                "failure",
                format!("Memory-curator backend review failed: {err}"),
                true,
                "error",
            )
            .await;
            push_curation_activity(
                state,
                "finish",
                "Finished standalone memory-curator automation run with backend failure",
                true,
            )
            .await;
            return Err(err);
        }
    };
    let record = payload.get("ledger_record").unwrap_or(&Value::Null);
    if record.get("fallback_status").and_then(Value::as_str) == Some("backend_failed_noop") {
        push_curation_activity_with_level(
            state,
            "failure",
            "Memory-curator backend was unavailable; recorded a no-op fallback run",
            true,
            "warning",
        )
        .await;
        push_curation_activity(
            state,
            "report",
            format!(
                "Memory-curator automation run {}: backend unavailable; no changes proposed",
                record
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
            ),
            true,
        )
        .await;
        push_curation_activity(
            state,
            "finish",
            "Finished standalone memory-curator automation run with no-op fallback",
            true,
        )
        .await;
        return Ok(payload);
    }
    push_curation_activity(
        state,
        "validation",
        format!(
            "Validated backend proposal: {} accepted op(s), {} rejected op(s)",
            record
                .get("accepted_count")
                .and_then(Value::as_i64)
                .unwrap_or_default(),
            record
                .get("rejected_count")
                .and_then(Value::as_i64)
                .unwrap_or_default()
        ),
        true,
    )
    .await;
    if record
        .get("rejected_count")
        .and_then(Value::as_i64)
        .unwrap_or_default()
        > 0
    {
        push_curation_activity_with_level(
            state,
            "rejection",
            format!(
                "Rejected {} backend-proposed op(s) during evidence validation",
                record
                    .get("rejected_count")
                    .and_then(Value::as_i64)
                    .unwrap_or_default()
            ),
            true,
            "warning",
        )
        .await;
    }
    let apply_policy = payload
        .get("report")
        .unwrap_or(&Value::Null)
        .get("automation_apply_policy")
        .cloned()
        .unwrap_or(Value::Null);
    let apply_decision = apply_policy
        .get("decision")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mutates_store = apply_policy
        .get("mutates_store")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    push_curation_activity(
        state,
        "apply",
        format!(
            "Memory-curator apply policy: {apply_decision}; store mutation {}",
            if mutates_store {
                "performed"
            } else {
                "not performed"
            }
        ),
        !mutates_store,
    )
    .await;
    push_curation_activity(
        state,
        "report",
        format!(
            "Memory-curator automation run {}: {} accepted op(s), {} rejected op(s)",
            record
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            record
                .get("accepted_count")
                .and_then(Value::as_i64)
                .unwrap_or_default(),
            record
                .get("rejected_count")
                .and_then(Value::as_i64)
                .unwrap_or_default()
        ),
        true,
    )
    .await;
    push_curation_activity(
        state,
        "finish",
        format!(
            "Finished standalone memory-curator automation run: {}",
            record
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        ),
        true,
    )
    .await;

    Ok(payload)
}

pub struct SessionReflectionRunRequest {
    pub provider: Option<String>,
    pub query: Option<String>,
    pub evidence_limit: Option<usize>,
    pub scope: Option<LcmScope>,
    pub session_id: Option<String>,
    pub include_summaries: Option<bool>,
    pub sort: Option<LcmGrepSort>,
    pub source: Option<String>,
    pub role: Option<String>,
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
}

pub struct SkillWritingRunRequest {
    pub provider: Option<String>,
    pub query: Option<String>,
    pub evidence_limit: Option<usize>,
}

pub async fn session_reflection_run_payload_with_run_id(
    state: &DashboardState,
    request: SessionReflectionRunRequest,
    run_id: Option<String>,
) -> Result<Value, String> {
    execute_dashboard_automation_run(state, move |state| async move {
        session_reflection_run_payload_with_run_id_direct(&state, request, run_id).await
    })
    .await
}

async fn session_reflection_run_payload_with_run_id_direct(
    state: &DashboardState,
    request: SessionReflectionRunRequest,
    run_id: Option<String>,
) -> Result<Value, String> {
    push_dashboard_automation_activity_start(
        state,
        "session-reflector",
        "Collecting session-reflector evidence from LCM search",
        "Preparing standalone session-reflector backend review",
    )
    .await;
    let payload = match execute_automation_task(
        state,
        DashboardAutomationTask::SessionReflection {
            provider: request.provider,
            query: request.query,
            evidence_limit: request.evidence_limit,
            scope: request.scope,
            session_id: request.session_id,
            include_summaries: request.include_summaries,
            sort: request.sort,
            source: request.source,
            role: request.role,
            start_time: request.start_time,
            end_time: request.end_time,
            run_id,
        },
    )
    .await
    {
        Ok(payload) => payload,
        Err(err) => {
            push_dashboard_automation_activity_failure(
                state,
                "session-reflector",
                format!("Session-reflector backend review failed: {err}"),
                "backend failure",
            )
            .await;
            return Err(err);
        }
    };
    push_dashboard_automation_activity_result(
        state,
        "session-reflector",
        payload.get("ledger_record").unwrap_or(&Value::Null),
    )
    .await;

    Ok(payload)
}

pub async fn skill_writing_run_payload_with_run_id(
    state: &DashboardState,
    request: SkillWritingRunRequest,
    run_id: Option<String>,
) -> Result<Value, String> {
    execute_dashboard_automation_run(state, move |state| async move {
        skill_writing_run_payload_with_run_id_direct(&state, request, run_id).await
    })
    .await
}

async fn skill_writing_run_payload_with_run_id_direct(
    state: &DashboardState,
    request: SkillWritingRunRequest,
    run_id: Option<String>,
) -> Result<Value, String> {
    push_dashboard_automation_activity_start(
        state,
        "skill-writer",
        "Collecting skill-writer evidence from LCM, managed skills, and usage telemetry",
        "Preparing standalone skill-writer backend review",
    )
    .await;
    let payload = match execute_automation_task(
        state,
        DashboardAutomationTask::SkillWriting {
            provider: request.provider,
            query: request.query,
            evidence_limit: request.evidence_limit,
            run_id,
        },
    )
    .await
    {
        Ok(payload) => payload,
        Err(err) => {
            push_dashboard_automation_activity_failure(
                state,
                "skill-writer",
                format!("Skill-writer backend review failed: {err}"),
                "backend failure",
            )
            .await;
            return Err(err);
        }
    };
    push_dashboard_automation_activity_result(
        state,
        "skill-writer",
        payload.get("ledger_record").unwrap_or(&Value::Null),
    )
    .await;

    Ok(payload)
}

async fn push_dashboard_automation_activity_start(
    state: &DashboardState,
    task_label: &str,
    evidence_message: &'static str,
    backend_message: &'static str,
) {
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
        format!("{evidence_message} for dashboard {task_label} automation run"),
        true,
    )
    .await;
    push_curation_activity(
        state,
        "backend",
        format!("{backend_message} for dashboard {task_label} automation run"),
        true,
    )
    .await;
}

async fn push_dashboard_automation_activity_failure(
    state: &DashboardState,
    task_label: &str,
    message: impl Into<String>,
    finish_reason: &str,
) {
    push_curation_activity_with_level(state, "failure", message, true, "error").await;
    push_curation_activity(
        state,
        "finish",
        format!("Finished dashboard {task_label} automation run with {finish_reason}"),
        true,
    )
    .await;
}

async fn push_dashboard_automation_activity_result(
    state: &DashboardState,
    task_label: &str,
    record: &Value,
) {
    let status = record
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let accepted_count = record
        .get("accepted_count")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let rejected_count = record
        .get("rejected_count")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    if status == "skipped" {
        let reason = record
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("skipped");
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
        return;
    }
    let mutates_store = automation_record_mutates_store(record);

    push_curation_activity(
        state,
        "validation",
        format!(
            "Validated dashboard {task_label} proposal: {accepted_count} accepted item(s), {rejected_count} rejected item(s)"
        ),
        true,
    )
    .await;
    push_curation_activity(
        state,
        "apply",
        format!(
            "Dashboard {task_label} automation run recorded store mutation {}",
            if mutates_store {
                "performed"
            } else {
                "not performed"
            }
        ),
        !mutates_store,
    )
    .await;
    push_curation_activity(
        state,
        "report",
        format!(
            "Dashboard {task_label} automation run {status}: {accepted_count} accepted item(s), {rejected_count} rejected item(s)"
        ),
        !mutates_store,
    )
    .await;
    push_curation_activity(
        state,
        "finish",
        format!("Finished dashboard {task_label} automation run: {status}"),
        !mutates_store,
    )
    .await;
}

fn automation_record_mutates_store(record: &Value) -> bool {
    let Some(report) = record.get("validation_report") else {
        return false;
    };
    report
        .pointer("/automation_apply_policy/mutates_store")
        .or_else(|| report.pointer("/session_fact_apply_policy/mutates_store"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

async fn execute_automation_task(
    state: &DashboardState,
    task: DashboardAutomationTask,
) -> Result<Value, String> {
    let executor = state
        .automation_executor
        .as_ref()
        .ok_or_else(|| "dashboard automation executor is unavailable".to_string())?;
    executor(task).await
}

pub fn automation_run_payload(
    run_id: &str,
    report: &Value,
    ledger_record: &crate::automation::run_ledger::AutomationRunLedgerRecord,
    backend_response: Option<&crate::automation::backend::AgentTaskResponse>,
) -> Value {
    json!({
        "run_id": run_id,
        "status": ledger_record.status,
        "report": report,
        "ledger_record": ledger_record,
        "backend_response": backend_response,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[tokio::test]
    async fn direct_writer_executes_operation_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&calls);
        let writer = direct_dashboard_automation_writer();

        let result = writer(Box::new(move || {
            Box::pin(async move {
                observed.fetch_add(1, Ordering::Relaxed);
                Ok(json!({ "status": "ok" }))
            })
        }))
        .await
        .expect("direct dashboard automation write should succeed");

        assert_eq!(result, json!({ "status": "ok" }));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }
}
