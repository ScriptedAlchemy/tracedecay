//! Dashboard endpoints for automation scheduler state and coarse controls.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use super::util::{http_detail, JsonError};
use super::DashboardState;
use crate::automation::backend::{task_key, AgentTaskKind};
use crate::automation::config::{effective_config, load_project_config, AutomationConfig};
use crate::automation::run_ledger::{load_run_records, AutomationRunLedgerRecord};
use crate::automation::scheduler::{
    load_scheduler_control, save_scheduler_control, schedule_decision, scheduler_control_path,
    AutomationSchedulerControl,
};
use crate::tracedecay::current_timestamp;
use crate::user_config::UserConfig;

type ApiResult = std::result::Result<Json<Value>, JsonError>;

pub(crate) async fn status(State(state): State<DashboardState>) -> ApiResult {
    scheduler_status_payload(&state).await
}

pub(crate) async fn pause(State(state): State<DashboardState>) -> ApiResult {
    set_scheduler_paused(&state, true).await?;
    scheduler_status_payload(&state).await
}

pub(crate) async fn resume(State(state): State<DashboardState>) -> ApiResult {
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
    let global = UserConfig::load().automation;
    let project = load_project_config(&state.dashboard_root)
        .await
        .map_err(|err| internal_error(&err))?;
    let effective =
        effective_config(&global, project.as_ref()).map_err(|err| internal_error(&err))?;
    let control = load_scheduler_control(&state.dashboard_root)
        .await
        .map_err(|err| internal_error(&err))?;
    let records = load_run_records(&state.dashboard_root, 200)
        .await
        .map_err(|err| internal_error(&err))?;
    let now = current_timestamp();
    Ok(Json(json!({
        "status": scheduler_status_label(&effective, control.paused),
        "paused": control.paused,
        "enabled": effective.enabled,
        "scheduler_tick_secs": effective.scheduler_tick_secs,
        "now": now,
        "project_config_path": crate::automation::config::project_config_path(&state.dashboard_root)
            .display()
            .to_string(),
        "control_path": scheduler_control_path(&state.dashboard_root)
            .display()
            .to_string(),
        "tasks": [
            task_status(&effective, control.paused, &records, now, AgentTaskKind::MemoryCurator),
            task_status(&effective, control.paused, &records, now, AgentTaskKind::SessionReflector),
            task_status(&effective, control.paused, &records, now, AgentTaskKind::SkillWriter),
        ],
    })))
}

fn task_status(
    config: &AutomationConfig,
    paused: bool,
    records: &[AutomationRunLedgerRecord],
    now: i64,
    task: AgentTaskKind,
) -> Value {
    let decision = if paused {
        crate::automation::scheduler::AutomationScheduleDecision::skipped("scheduler_paused")
    } else {
        schedule_decision(config, task, records, now)
    };
    let latest_scheduler = records
        .iter()
        .filter(|record| {
            record.task == task
                && record.trigger == crate::automation::run_ledger::AutomationTrigger::Scheduler
        })
        .max_by_key(|record| record.completed_at.parse::<i64>().ok().unwrap_or(0));
    json!({
        "task": task_key(task),
        "due": decision.is_due(),
        "skip_reason": decision.skip_reason(),
        "last_scheduler_run": latest_scheduler,
    })
}

fn scheduler_status_label(config: &AutomationConfig, paused: bool) -> &'static str {
    if paused {
        return "paused";
    }
    if !config.enabled {
        return "automation_disabled";
    }
    if config.host_mode == crate::automation::config::AutomationHostMode::DelegatedHost {
        return "delegated_host";
    }
    if config.backend == crate::automation::config::AutomationBackend::Disabled {
        return "backend_disabled";
    }
    "configured"
}

fn internal_error(err: &impl ToString) -> JsonError {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(http_detail(&err.to_string())),
    )
}
