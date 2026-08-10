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
use tracedecay_agent_hosts::automation::run_ledger::{AutomationRunLedgerRecord, load_run_records};
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
    let records = load_run_records(&state.dashboard_root, 200)
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
                &records,
                activity,
                now,
                AgentTaskKind::MemoryCurator,
            )?,
            task_status(
                &effective,
                control.paused,
                &records,
                activity,
                now,
                AgentTaskKind::SessionReflector,
            )?,
            task_status(
                &effective,
                control.paused,
                &records,
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
    records: &[AutomationRunLedgerRecord],
    activity: SessionActivity,
    now: i64,
    task: AgentTaskKind,
) -> std::result::Result<AutomationTaskStatusV1, JsonError> {
    let decision = if paused {
        tracedecay_agent_hosts::automation::scheduler::AutomationScheduleDecision::skipped(
            "scheduler_paused",
        )
    } else {
        schedule_decision(config, task, records, activity, now)
    };
    let latest_scheduler = records
        .iter()
        .filter(|record| {
            record.task == task
                && record.trigger
                    == tracedecay_agent_hosts::automation::run_ledger::AutomationTrigger::Scheduler
        })
        .max_by(|left, right| left.completed_at.cmp(&right.completed_at));
    Ok(AutomationTaskStatusV1 {
        task: task_key(task).to_string(),
        due: decision.is_due(),
        skip_reason: decision.skip_reason().map(str::to_string),
        last_scheduler_run: latest_scheduler
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
