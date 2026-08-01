use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::{Value, json};

use super::DashboardState;
use super::memory_service::{push_curation_activity, push_curation_activity_with_level};
use tracedecay_sessions::runtime::lcm::{LcmGrepSort, LcmScope};

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

pub fn standalone_dashboard_automation_writer() -> DashboardAutomationWriter {
    let writer = Arc::new(tokio::sync::Mutex::new(()));
    Arc::new(move |operation| {
        let writer = Arc::clone(&writer);
        Box::pin(async move {
            let _guard = writer.lock().await;
            operation().await
        })
    })
}

pub async fn execute_dashboard_automation_write<Operation, OperationFuture>(
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
    execute_dashboard_automation_write(state, move |state| async move {
        Box::pin(memory_curator_run_payload_with_run_id_direct(
            &state, request, run_id,
        ))
        .await
    })
    .await
}

async fn memory_curator_run_payload_with_run_id_direct(
    state: &DashboardState,
    request: MemoryCuratorRunRequest,
    run_id: Option<String>,
) -> Result<Value, String> {
    use tracedecay_agent_hosts::automation::run_ledger::AutomationTrigger;
    use tracedecay_agent_hosts::automation::runner::{
        MemoryCuratorAutomationOptions, run_memory_curator_with_backend,
    };

    push_curation_activity(
        state,
        "queued",
        "Queued standalone memory-curator automation run",
        true,
    )
    .await;
    let run_context = match dashboard_automation_run_context(state).await {
        Ok(context) => context,
        Err(err) => {
            push_curation_activity_with_level(
                state,
                "failure",
                format!("Could not prepare memory-curator backend context: {err}"),
                true,
                "error",
            )
            .await;
            push_curation_activity(
                state,
                "finish",
                "Finished standalone memory-curator automation run with setup failure",
                true,
            )
            .await;
            return Err(err);
        }
    };

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
    let run = match run_memory_curator_with_backend(
        &run_context.cg,
        &run_context.config,
        &run_context.backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::Dashboard,
            run_id,
            max_clusters: request.max_clusters,
            min_confidence: request.min_confidence,
        },
    )
    .await
    {
        Ok(run) => run,
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
            return Err(err.to_string());
        }
    };
    if run.ledger_record.fallback_status.as_deref() == Some("backend_failed_noop") {
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
                run.ledger_record.status.as_str()
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
        return Ok(automation_run_payload(
            &run.run_id,
            &run.report,
            &run.ledger_record,
            run.backend_response.as_ref(),
        ));
    }
    push_curation_activity(
        state,
        "validation",
        format!(
            "Validated backend proposal: {} accepted op(s), {} rejected op(s)",
            run.ledger_record.accepted_count, run.ledger_record.rejected_count
        ),
        true,
    )
    .await;
    if run.ledger_record.rejected_count > 0 {
        push_curation_activity_with_level(
            state,
            "rejection",
            format!(
                "Rejected {} backend-proposed op(s) during evidence validation",
                run.ledger_record.rejected_count
            ),
            true,
            "warning",
        )
        .await;
    }
    let apply_policy = run
        .report
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
            run.ledger_record.status.as_str(),
            run.ledger_record.accepted_count,
            run.ledger_record.rejected_count
        ),
        true,
    )
    .await;
    push_curation_activity(
        state,
        "finish",
        format!(
            "Finished standalone memory-curator automation run: {}",
            run.ledger_record.status.as_str()
        ),
        true,
    )
    .await;

    Ok(automation_run_payload(
        &run.run_id,
        &run.report,
        &run.ledger_record,
        run.backend_response.as_ref(),
    ))
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
    execute_dashboard_automation_write(state, move |state| async move {
        Box::pin(session_reflection_run_payload_with_run_id_direct(
            &state, request, run_id,
        ))
        .await
    })
    .await
}

async fn session_reflection_run_payload_with_run_id_direct(
    state: &DashboardState,
    request: SessionReflectionRunRequest,
    run_id: Option<String>,
) -> Result<Value, String> {
    use tracedecay_agent_hosts::automation::run_ledger::AutomationTrigger;
    use tracedecay_agent_hosts::automation::runner::{
        SessionReflectorAutomationOptions, run_session_reflector_with_backend,
    };

    push_dashboard_automation_activity_start(
        state,
        "session-reflector",
        "Collecting session-reflector evidence from LCM search",
        "Preparing standalone session-reflector backend review",
    )
    .await;
    let run_context = match dashboard_automation_run_context(state).await {
        Ok(context) => context,
        Err(err) => {
            push_dashboard_automation_activity_failure(
                state,
                "session-reflector",
                format!("Could not prepare session-reflector backend context: {err}"),
                "setup failure",
            )
            .await;
            return Err(err);
        }
    };
    let mut options = SessionReflectorAutomationOptions {
        trigger: AutomationTrigger::Dashboard,
        run_id,
        ..SessionReflectorAutomationOptions::default()
    };
    if let Some(provider) = request.provider {
        options.provider = provider;
    }
    if let Some(query) = request.query {
        options.query = query;
    }
    if let Some(evidence_limit) = request.evidence_limit {
        options.evidence_limit = evidence_limit;
    }
    if let Some(scope) = request.scope {
        options.scope = scope;
    }
    if let Some(session_id) = request.session_id {
        options.session_id = Some(session_id);
    }
    if let Some(include_summaries) = request.include_summaries {
        options.include_summaries = include_summaries;
    }
    if let Some(sort) = request.sort {
        options.sort = sort;
    }
    if let Some(source) = request.source {
        options.source = Some(source);
    }
    if let Some(role) = request.role {
        options.role = Some(role);
    }
    options.start_time = request.start_time;
    options.end_time = request.end_time;
    let run = match run_session_reflector_with_backend(
        &run_context.cg,
        &run_context.config,
        &run_context.backend,
        options,
    )
    .await
    {
        Ok(run) => run,
        Err(err) => {
            push_dashboard_automation_activity_failure(
                state,
                "session-reflector",
                format!("Session-reflector backend review failed: {err}"),
                "backend failure",
            )
            .await;
            return Err(err.to_string());
        }
    };
    push_dashboard_automation_activity_result(state, "session-reflector", &run.ledger_record).await;

    Ok(automation_run_payload(
        &run.run_id,
        &run.report,
        &run.ledger_record,
        run.backend_response.as_ref(),
    ))
}

pub async fn skill_writing_run_payload_with_run_id(
    state: &DashboardState,
    request: SkillWritingRunRequest,
    run_id: Option<String>,
) -> Result<Value, String> {
    execute_dashboard_automation_write(state, move |state| async move {
        Box::pin(skill_writing_run_payload_with_run_id_direct(
            &state, request, run_id,
        ))
        .await
    })
    .await
}

async fn skill_writing_run_payload_with_run_id_direct(
    state: &DashboardState,
    request: SkillWritingRunRequest,
    run_id: Option<String>,
) -> Result<Value, String> {
    use tracedecay_agent_hosts::automation::run_ledger::AutomationTrigger;
    use tracedecay_agent_hosts::automation::runner::{
        SkillWriterAutomationOptions, run_skill_writer_with_backend,
    };

    push_dashboard_automation_activity_start(
        state,
        "skill-writer",
        "Collecting skill-writer evidence from LCM, managed skills, and usage telemetry",
        "Preparing standalone skill-writer backend review",
    )
    .await;
    let run_context = match dashboard_automation_run_context(state).await {
        Ok(context) => context,
        Err(err) => {
            push_dashboard_automation_activity_failure(
                state,
                "skill-writer",
                format!("Could not prepare skill-writer backend context: {err}"),
                "setup failure",
            )
            .await;
            return Err(err);
        }
    };
    let mut options = SkillWriterAutomationOptions {
        trigger: AutomationTrigger::Dashboard,
        run_id,
        profile_root: None,
        ..SkillWriterAutomationOptions::default()
    };
    if let Some(provider) = request.provider {
        options.provider = provider;
    }
    if let Some(query) = request.query {
        options.query = query;
    }
    if let Some(evidence_limit) = request.evidence_limit {
        options.evidence_limit = evidence_limit;
    }
    let run = match run_skill_writer_with_backend(
        &run_context.cg,
        &run_context.config,
        &run_context.backend,
        options,
    )
    .await
    {
        Ok(run) => run,
        Err(err) => {
            push_dashboard_automation_activity_failure(
                state,
                "skill-writer",
                format!("Skill-writer backend review failed: {err}"),
                "backend failure",
            )
            .await;
            return Err(err.to_string());
        }
    };
    push_dashboard_automation_activity_result(state, "skill-writer", &run.ledger_record).await;

    Ok(automation_run_payload(
        &run.run_id,
        &run.report,
        &run.ledger_record,
        run.backend_response.as_ref(),
    ))
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
    record: &tracedecay_agent_hosts::automation::run_ledger::AutomationRunLedgerRecord,
) {
    if record.status == tracedecay_agent_hosts::automation::run_ledger::AutomationRunStatus::Skipped
    {
        let reason = record.error.as_deref().unwrap_or("skipped");
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
            "Validated dashboard {task_label} proposal: {} accepted item(s), {} rejected item(s)",
            record.accepted_count, record.rejected_count
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
            "Dashboard {task_label} automation run {}: {} accepted item(s), {} rejected item(s)",
            record.status.as_str(),
            record.accepted_count,
            record.rejected_count
        ),
        !mutates_store,
    )
    .await;
    push_curation_activity(
        state,
        "finish",
        format!(
            "Finished dashboard {task_label} automation run: {}",
            record.status.as_str()
        ),
        !mutates_store,
    )
    .await;
}

fn automation_record_mutates_store(
    record: &tracedecay_agent_hosts::automation::run_ledger::AutomationRunLedgerRecord,
) -> bool {
    let Some(report) = record.validation_report.as_ref() else {
        return false;
    };
    report
        .pointer("/automation_apply_policy/mutates_store")
        .or_else(|| report.pointer("/session_fact_apply_policy/mutates_store"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

struct DashboardAutomationRunContext {
    cg: Arc<crate::tracedecay::TraceDecay>,
    config: tracedecay_agent_hosts::automation::config::AutomationConfig,
    backend: tracedecay_agent_hosts::automation::backend::CodexAppServerBackend,
}

async fn dashboard_automation_run_context(
    state: &DashboardState,
) -> Result<DashboardAutomationRunContext, String> {
    use tracedecay_agent_hosts::automation::backend::CodexAppServerBackend;
    use tracedecay_agent_hosts::automation::config::{
        AutomationBackend, effective_config, load_project_config,
    };
    let cg = state
        .project_graph
        .as_ref()
        .map(Arc::clone)
        .ok_or_else(|| "retained dashboard project graph is unavailable".to_string())?;
    let global = crate::user_config::UserConfig::load().automation;
    let project = load_project_config(&state.dashboard_root)
        .await
        .map_err(|e| e.to_string())?;
    let config = effective_config(&global, project.as_ref()).map_err(|e| e.to_string())?;
    if config.enabled && config.backend == AutomationBackend::ExternalCommand {
        return Err("automation backend external_command is not implemented yet".to_string());
    }
    let backend = CodexAppServerBackend::from_automation_config(&config);
    Ok(DashboardAutomationRunContext {
        cg,
        config,
        backend,
    })
}

fn automation_run_payload(
    run_id: &str,
    report: &Value,
    ledger_record: &tracedecay_agent_hosts::automation::run_ledger::AutomationRunLedgerRecord,
    backend_response: Option<&tracedecay_agent_hosts::automation::backend::AgentTaskResponse>,
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
    async fn standalone_writer_executes_operation_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&calls);
        let writer = standalone_dashboard_automation_writer();

        let result = writer(Box::new(move || {
            Box::pin(async move {
                observed.fetch_add(1, Ordering::Relaxed);
                Ok(json!({ "status": "ok" }))
            })
        }))
        .await
        .expect("standalone dashboard automation write should succeed");

        assert_eq!(result, json!({ "status": "ok" }));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn standalone_writer_serializes_operations() {
        let writer = standalone_dashboard_automation_writer();
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let operation = |active: Arc<AtomicUsize>, maximum: Arc<AtomicUsize>| {
            Box::new(move || {
                Box::pin(async move {
                    let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                    maximum.fetch_max(current, Ordering::SeqCst);
                    tokio::task::yield_now().await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok(Value::Null)
                }) as DashboardAutomationWriteFuture
            }) as DashboardAutomationWriteOperation
        };

        let first = writer(operation(Arc::clone(&active), Arc::clone(&maximum)));
        let second = writer(operation(active, Arc::clone(&maximum)));
        let (first, second) = tokio::join!(first, second);

        first.expect("first standalone write");
        second.expect("second standalone write");
        assert_eq!(maximum.load(Ordering::SeqCst), 1);
    }
}
