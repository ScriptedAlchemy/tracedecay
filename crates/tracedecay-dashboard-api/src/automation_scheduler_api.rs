//! Dashboard endpoints for automation scheduler state and coarse controls.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde_json::{Value, json};

use super::DashboardState;
use super::util::{JsonError, http_detail};
use crate::automation::backend::{AgentTaskKind, task_key};
use crate::automation::config::{AutomationConfig, effective_config, load_project_config};
use crate::automation::run_ledger::{AutomationRunLedgerRecord, load_run_records};
use crate::automation::scheduler::{
    AutomationSchedulerControl, SessionActivity, load_scheduler_control, load_session_activity,
    save_scheduler_control, schedule_decision, scheduler_control_path,
};
use crate::automation::staged_notice::{AutomationPendingCounts, count_pending_automation_output};
use crate::tracedecay::current_timestamp;
use crate::user_config::UserConfig;

type ApiResult = std::result::Result<Json<Value>, JsonError>;

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
    // Additive pending-output counts. Fact proposals stay telemetry/backcompat;
    // pending skills are the only human-review badge input.
    let pending = match crate::storage::default_profile_root() {
        Ok(profile_root) => {
            count_pending_automation_output(&state.dashboard_root, &profile_root).await
        }
        Err(_) => AutomationPendingCounts::default(),
    };
    let now = current_timestamp();
    let activity =
        load_session_activity(&state.store_root.join(crate::storage::SESSIONS_DB_FILENAME)).await;
    Ok(Json(json!({
        "status": scheduler_status_label(&effective, control.paused),
        "paused": control.paused,
        "pending_fact_proposals": pending.pending_fact_proposals,
        "pending_skills": pending.pending_skills,
        "enabled": effective.enabled,
        "scheduler_tick_secs": effective.scheduler_tick_secs,
        "now": now,
        "last_session_activity": activity.last_activity_secs,
        "project_config_path": crate::automation::config::project_config_path(&state.dashboard_root)
            .display()
            .to_string(),
        "control_path": scheduler_control_path(&state.dashboard_root)
            .display()
            .to_string(),
        "tasks": [
            task_status(&effective, control.paused, &records, activity, now, AgentTaskKind::MemoryCurator),
            task_status(&effective, control.paused, &records, activity, now, AgentTaskKind::SessionReflector),
            task_status(&effective, control.paused, &records, activity, now, AgentTaskKind::SkillWriter),
        ],
    })))
}

fn task_status(
    config: &AutomationConfig,
    paused: bool,
    records: &[AutomationRunLedgerRecord],
    activity: SessionActivity,
    now: i64,
    task: AgentTaskKind,
) -> Value {
    let decision = if paused {
        crate::automation::scheduler::AutomationScheduleDecision::skipped("scheduler_paused")
    } else {
        schedule_decision(config, task, records, activity, now)
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
