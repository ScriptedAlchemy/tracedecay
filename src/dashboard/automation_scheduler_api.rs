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
use crate::automation::staged_notice::{
    AutomationPendingCounts, PendingReviewCount, count_pending_fact_proposals,
    count_pending_managed_skills,
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
    let pending = pending_review_counts(state).await;
    let now = current_timestamp();
    let activity = match state.lcm_db.as_deref() {
        Some(sessions_db) => load_session_activity(sessions_db).await,
        None => SessionActivity::none(),
    };
    Ok(Json(json!({
        "status": scheduler_status_label(&effective, control.paused),
        "paused": control.paused,
        // Null, never zero, when the queue could not be read; `pending_review`
        // carries which reading each figure actually is.
        "pending_fact_proposals": pending.fact_proposals.count(),
        "pending_skills": pending.skills.count(),
        "pending_review": {
            "fact_proposals": pending_review_json(&pending.fact_proposals),
            "skills": pending_review_json(&pending.skills),
        },
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

/// Reads the two human-review queues, each from its own authority.
///
/// Both reads used to be gated behind one `match` whose failure arm produced
/// zeroes, so an unresolvable profile root or an unmounted fact authority
/// served `pending_fact_proposals: 0, pending_skills: 0` under HTTP 200 — a
/// report that nothing awaits human approval. They are independent reads now,
/// and a queue that cannot be read says so.
async fn pending_review_counts(state: &DashboardState) -> AutomationPendingCounts {
    let fact_proposals = match crate::tracedecay::facts::memory_application_for_db(
        state.memory_owner.clone(),
        state.mem_db.as_ref(),
    ) {
        Ok(memory) => count_pending_fact_proposals(&memory).await,
        Err(error) => PendingReviewCount::unreadable(format!(
            "the project fact authority is not available: {error}"
        )),
    };
    let skills = match crate::storage::default_profile_root() {
        Ok(profile_root) => count_pending_managed_skills(&profile_root).await,
        Err(error) => PendingReviewCount::unreadable(format!(
            "the user profile root could not be resolved: {error}"
        )),
    };
    AutomationPendingCounts {
        fact_proposals,
        skills,
    }
}

/// The per-queue reading, on the dashboard's evidence vocabulary: `measured`
/// carries a real count, `unreadable` carries the reason and no count.
fn pending_review_json(count: &PendingReviewCount) -> Value {
    match count {
        PendingReviewCount::Counted(count) => json!({
            "state": "measured",
            "count": count,
            "reason": Value::Null,
        }),
        PendingReviewCount::Unreadable(reason) => json!({
            "state": "unreadable",
            "count": Value::Null,
            "reason": reason,
        }),
    }
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
