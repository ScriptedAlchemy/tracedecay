//! Dashboard endpoints for automation scheduler state and coarse controls.

use axum::Json;
use axum::extract::State;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

use super::DashboardState;
use super::automation_config_api::effective_automation_config;
use super::util::{JsonError, internal_error};
use tracedecay_agent_hosts::automation::backend::{AgentTaskKind, task_key};
use tracedecay_agent_hosts::automation::config::AutomationConfig;
use tracedecay_agent_hosts::automation::run_ledger::{
    AutomationRunLedgerTaskSummary, load_run_ledger_task_summary,
};
use tracedecay_agent_hosts::automation::scheduler::{
    AutomationSchedulerControl, SessionActivity, load_scheduler_control, load_session_activity,
    save_scheduler_control, schedule_decision, scheduler_control_path,
};
use tracedecay_runtime_core::tracedecay::current_timestamp;

type ApiResult = std::result::Result<Json<AutomationSchedulerStatusV1>, JsonError>;

/// The scheduler reading served by `status`, `pause`, and `resume`.
///
/// Automation is autonomous: the status has no pending-review counters. The
/// last run record carries the validation, repair, application, quarantine,
/// and deployment receipts that describe what actually happened.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct AutomationSchedulerStatusV1 {
    /// `paused`, `automation_disabled`, `delegated_host`, `backend_disabled`,
    /// or `configured`.
    pub status: String,
    pub paused: bool,
    pub enabled: bool,
    pub scheduler_tick_secs: u64,
    pub now: i64,
    pub last_session_activity: Option<i64>,
    pub configuration_revision_id: String,
    pub control_path: String,
    pub tasks: Vec<AutomationTaskStatusV1>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(super) struct AutomationTaskStatusV1 {
    pub task: String,
    pub due: bool,
    pub skip_reason: Option<String>,
    /// The most recent scheduler-triggered ledger record. Its run artifacts
    /// remain the canonical detailed receipt surface.
    pub last_scheduler_run: Option<Value>,
}

pub async fn status(State(state): State<DashboardState>) -> ApiResult {
    scheduler_status_payload(&state).await
}

pub async fn pause(State(state): State<DashboardState>) -> ApiResult {
    set_scheduler_paused(&state, true).await?;
    scheduler_status_payload(&state).await
}

pub async fn resume(State(state): State<DashboardState>) -> ApiResult {
    set_scheduler_paused(&state, false).await?;
    scheduler_status_payload(&state).await
}

async fn set_scheduler_paused(
    state: &DashboardState,
    paused: bool,
) -> std::result::Result<(), JsonError> {
    save_scheduler_control(
        &state.dashboard_root,
        &AutomationSchedulerControl { paused },
    )
    .await
    .map_err(|err| internal_error(&err))
}

async fn scheduler_status_payload(state: &DashboardState) -> ApiResult {
    let (configuration_revision_id, effective) =
        effective_automation_config(state).map_err(|err| internal_error(&err))?;
    let control = load_scheduler_control(&state.dashboard_root)
        .await
        .map_err(|err| internal_error(&err))?;
    let memory_summary = load_run_ledger_task_summary(
        &state.dashboard_root,
        AgentTaskKind::MemoryCurator,
        task_key(AgentTaskKind::MemoryCurator),
    )
    .await
    .map_err(|err| internal_error(&err))?;
    let session_summary = load_run_ledger_task_summary(
        &state.dashboard_root,
        AgentTaskKind::SessionReflector,
        task_key(AgentTaskKind::SessionReflector),
    )
    .await
    .map_err(|err| internal_error(&err))?;
    let skill_summary = load_run_ledger_task_summary(
        &state.dashboard_root,
        AgentTaskKind::SkillWriter,
        task_key(AgentTaskKind::SkillWriter),
    )
    .await
    .map_err(|err| internal_error(&err))?;
    let now = current_timestamp();
    let activity = match state.lcm_db.as_deref() {
        Some(sessions_db) => load_session_activity(sessions_db).await,
        None => SessionActivity::none(),
    };
    Ok(Json(AutomationSchedulerStatusV1 {
        status: scheduler_status_label(&effective, control.paused).to_string(),
        paused: control.paused,
        enabled: effective.enabled,
        scheduler_tick_secs: effective.scheduler_tick_secs,
        now,
        last_session_activity: activity.last_activity_secs,
        configuration_revision_id: configuration_revision_id.as_str().to_owned(),
        control_path: scheduler_control_path(&state.dashboard_root)
            .display()
            .to_string(),
        tasks: vec![
            task_status(
                &effective,
                control.paused,
                &memory_summary,
                activity,
                now,
                AgentTaskKind::MemoryCurator,
            )?,
            task_status(
                &effective,
                control.paused,
                &session_summary,
                activity,
                now,
                AgentTaskKind::SessionReflector,
            )?,
            task_status(
                &effective,
                control.paused,
                &skill_summary,
                activity,
                now,
                AgentTaskKind::SkillWriter,
            )?,
        ],
    }))
}

fn task_status(
    config: &AutomationConfig,
    paused: bool,
    summary: &AutomationRunLedgerTaskSummary,
    activity: SessionActivity,
    now: i64,
    task: AgentTaskKind,
) -> std::result::Result<AutomationTaskStatusV1, JsonError> {
    let decision = if paused {
        tracedecay_agent_hosts::automation::scheduler::AutomationScheduleDecision::skipped(
            "scheduler_paused",
        )
    } else {
        schedule_decision(config, task, summary.records(), activity, now)
    };
    Ok(AutomationTaskStatusV1 {
        task: task_key(task).to_string(),
        due: decision.is_due(),
        skip_reason: decision.skip_reason().map(str::to_string),
        last_scheduler_run: summary
            .latest_scheduler_activity()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|error| internal_error(&error))?,
    })
}

fn scheduler_status_label(config: &AutomationConfig, paused: bool) -> &'static str {
    if paused {
        return "paused";
    }
    if !config.enabled {
        return "automation_disabled";
    }
    if config.host_mode
        == tracedecay_agent_hosts::automation::config::AutomationHostMode::DelegatedHost
    {
        return "delegated_host";
    }
    if config.backend == tracedecay_agent_hosts::automation::config::AutomationBackend::Disabled {
        return "backend_disabled";
    }
    "configured"
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tracedecay_agent_hosts::automation::backend::AgentTaskFailureClass;
    use tracedecay_agent_hosts::automation::config::{AutomationBackend, AutomationTaskConfig};
    use tracedecay_agent_hosts::automation::run_ledger::{
        AutomationRunLedgerRecord, AutomationRunStatus, AutomationTrigger, run_ledger_path,
    };

    fn record(
        run_id: &str,
        task: AgentTaskKind,
        status: AutomationRunStatus,
        completed_at: i64,
        completed_at_micros: i64,
    ) -> AutomationRunLedgerRecord {
        serde_json::from_value(serde_json::json!({
            "schema_version": 2,
            "run_id": run_id,
            "trigger": AutomationTrigger::Scheduler,
            "task": task,
            "task_key": task_key(task),
            "backend": "codex_app_server",
            "status": status,
            "accepted_count": 0,
            "rejected_count": 0,
            "started_at": completed_at.to_string(),
            "completed_at": completed_at.to_string(),
            "completed_at_micros": completed_at_micros,
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn status_uses_full_canonical_summary_for_decision_and_last_scheduler_run() {
        let temp = tempfile::TempDir::new().unwrap();
        let mut older_success = record(
            "a-success",
            AgentTaskKind::MemoryCurator,
            AutomationRunStatus::Succeeded,
            100,
            100_000_100,
        );
        let mut later_failure = record(
            "z-failure",
            AgentTaskKind::MemoryCurator,
            AutomationRunStatus::Failed,
            100,
            100_000_900,
        );
        later_failure.error = Some("the request is permanently invalid".to_string());
        later_failure.error_classification = Some(AgentTaskFailureClass::Permanent);
        later_failure.error_retryable = Some(false);
        older_success.model = Some("older".to_string());
        let mut rows = vec![older_success, later_failure];
        rows.extend((0..201).map(|index| {
            record(
                &format!("unrelated-{index}"),
                AgentTaskKind::SkillWriter,
                AutomationRunStatus::Succeeded,
                101 + index,
                (101 + index) * 1_000_000,
            )
        }));
        let body = rows
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join("\n")
            + "\n";
        std::fs::write(run_ledger_path(temp.path()), body).unwrap();

        let summary = load_run_ledger_task_summary(
            temp.path(),
            AgentTaskKind::MemoryCurator,
            task_key(AgentTaskKind::MemoryCurator),
        )
        .await
        .unwrap();
        let mut config = AutomationConfig::default();
        config.enabled = true;
        config.backend = AutomationBackend::CodexAppServer;
        config.tasks.memory_curator = AutomationTaskConfig {
            enabled: true,
            schedule: Some("daily".to_string()),
            ..AutomationTaskConfig::default()
        };

        let status = task_status(
            &config,
            false,
            &summary,
            SessionActivity::none(),
            150,
            AgentTaskKind::MemoryCurator,
        )
        .unwrap();

        assert!(!status.due);
        assert_eq!(
            status.skip_reason.as_deref(),
            Some("scheduler_non_retryable_failure")
        );
        assert_eq!(status.last_scheduler_run.unwrap()["run_id"], "z-failure");
    }
}
