use std::path::PathBuf;

use serde_json::{json, Value};

use super::memory_service::{push_curation_activity, push_curation_activity_with_level};
use super::DashboardState;
use crate::sessions::lcm::{LcmGrepSort, LcmScope};

pub(crate) struct MemoryCuratorRunRequest {
    pub max_clusters: usize,
    pub min_confidence: f64,
}

pub(crate) async fn curation_agent_plan_payload(
    state: &DashboardState,
    max_clusters: usize,
    min_confidence: f64,
) -> Result<Value, String> {
    Box::pin(curation_agent_plan_payload_with_run_id(
        state,
        MemoryCuratorRunRequest {
            max_clusters,
            min_confidence,
        },
        None,
    ))
    .await
}

pub(crate) async fn curation_agent_plan_payload_with_run_id(
    state: &DashboardState,
    request: MemoryCuratorRunRequest,
    run_id: Option<String>,
) -> Result<Value, String> {
    use crate::automation::run_ledger::AutomationTrigger;
    use crate::automation::runner::{
        run_memory_curator_with_backend, MemoryCuratorAutomationOptions,
    };

    push_curation_activity(
        state,
        "queued",
        "Queued standalone memory-curator agent plan",
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
                "Finished standalone memory-curator agent plan with setup failure",
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
                "Finished standalone memory-curator agent plan with backend failure",
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
                "Agent plan {}: backend unavailable; no changes proposed",
                run.ledger_record.status.as_str()
            ),
            true,
        )
        .await;
        push_curation_activity(
            state,
            "finish",
            "Finished standalone memory-curator agent plan with no-op fallback",
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
            "Agent plan {}: {} accepted op(s), {} rejected op(s)",
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
            "Finished standalone memory-curator agent plan: {}",
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

pub(crate) struct SessionReflectionRunRequest {
    pub provider: Option<String>,
    pub query: Option<String>,
    pub evidence_limit: Option<usize>,
    pub storage_scope: Option<String>,
    pub hermes_home: Option<PathBuf>,
    pub scope: Option<LcmScope>,
    pub session_id: Option<String>,
    pub include_summaries: Option<bool>,
    pub sort: Option<LcmGrepSort>,
    pub source: Option<String>,
    pub role: Option<String>,
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
}

pub(crate) struct SkillWritingRunRequest {
    pub provider: Option<String>,
    pub query: Option<String>,
    pub evidence_limit: Option<usize>,
    pub storage_scope: Option<String>,
    pub hermes_home: Option<PathBuf>,
}

pub(crate) async fn session_reflection_run_payload_with_run_id(
    state: &DashboardState,
    request: SessionReflectionRunRequest,
    run_id: Option<String>,
) -> Result<Value, String> {
    use crate::automation::run_ledger::AutomationTrigger;
    use crate::automation::runner::{
        run_session_reflector_with_backend, SessionReflectorAutomationOptions,
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
    if let Some(storage_scope) = request.storage_scope {
        options.storage_scope = storage_scope;
    }
    if let Some(hermes_home) = request.hermes_home {
        options.hermes_home = Some(hermes_home);
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

pub(crate) async fn skill_writing_run_payload_with_run_id(
    state: &DashboardState,
    request: SkillWritingRunRequest,
    run_id: Option<String>,
) -> Result<Value, String> {
    use crate::automation::run_ledger::AutomationTrigger;
    use crate::automation::runner::{run_skill_writer_with_backend, SkillWriterAutomationOptions};

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
    if let Some(storage_scope) = request.storage_scope {
        options.storage_scope = storage_scope;
    }
    if let Some(hermes_home) = request.hermes_home {
        options.hermes_home = Some(hermes_home);
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
    record: &crate::automation::run_ledger::AutomationRunLedgerRecord,
) {
    if record.status == crate::automation::run_ledger::AutomationRunStatus::Skipped {
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
        format!("Dashboard {task_label} run kept mutations gated behind approval controls"),
        true,
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
        true,
    )
    .await;
    push_curation_activity(
        state,
        "finish",
        format!(
            "Finished dashboard {task_label} automation run: {}",
            record.status.as_str()
        ),
        true,
    )
    .await;
}

struct DashboardAutomationRunContext {
    cg: crate::tracedecay::TraceDecay,
    config: crate::automation::config::AutomationConfig,
    backend: crate::automation::backend::CodexAppServerBackend,
}

async fn dashboard_automation_run_context(
    state: &DashboardState,
) -> Result<DashboardAutomationRunContext, String> {
    use crate::automation::backend::CodexAppServerBackend;
    use crate::automation::config::{effective_config, load_project_config, AutomationBackend};
    use crate::tracedecay::TraceDecay;

    let cg = TraceDecay::open(&state.project_root)
        .await
        .map_err(|e| e.to_string())?;
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
    ledger_record: &crate::automation::run_ledger::AutomationRunLedgerRecord,
    backend_response: Option<&crate::automation::backend::AgentTaskResponse>,
) -> Value {
    json!({
        "run_id": run_id,
        "dry_run": true,
        "status": ledger_record.status,
        "report": report,
        "ledger_record": ledger_record,
        "backend_response": backend_response,
    })
}
